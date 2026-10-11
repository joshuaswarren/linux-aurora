// SPDX-License-Identifier: GPL-2.0-only
//! Explicit render qualification input: application buffers plus a UAPI command.
//! Kernel queues, pool state and page tables are always constructed by the driver.
use super::{command::{self,Access,AddressSpace},fw::render::RenderPass,
    object::{CpuMap,KernelObject}};
use crate::{driver::AsahiDevice,mmu};
use kernel::{firmware::Firmware,prelude::*};
use core::sync::atomic::Ordering;

struct Reader<'a>(&'a[u8]);
impl<'a> Reader<'a> {
    fn take(&mut self,n:usize)->Result<&'a[u8]>{
        let out=self.0.get(..n).ok_or(EINVAL)?;self.0=&self.0[n..];Ok(out)
    }
    fn u32(&mut self)->Result<u32>{Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_|EINVAL)?))}
    fn u64(&mut self)->Result<u64>{Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_|EINVAL)?))}
}
struct Space<'a>(&'a mmu::Vm);
impl AddressSpace for Space<'_> {
    fn covers(&self,a:u64,n:u64,access:Access)->bool{
        self.0.covers_range(a,n,true,matches!(access,Access::Write|Access::ReadWrite))
    }
}
pub(super) struct Input {
    aliases:KVec<mmu::KernelMapping>,
    objects:KVec<KernelObject>,
    pub(super) pass:RenderPass,
    pixel_object:usize,
    pixel_offset:usize,
    pixel_words:usize,
    expected:u32,
}
impl Input {
    pub(super) fn load(dev:&AsahiDevice,vm:&mmu::Vm,range:core::ops::Range<u64>)->Result<Self>{
        let data=Firmware::request(c"asahi/m5-render-input.bin",dev.as_ref())?;
        let mut r=Reader(data.data());
        if r.take(8)?!=b"M5RTEST1" || r.u32()?!=1 {return Err(EINVAL);}
        let count=r.u32()? as usize;let mappings=r.u32()? as usize;let command_bytes=r.u32()? as usize;
        let usc=r.u64()?;let output=r.u64()?;let output_bytes=r.u32()? as usize;let expected=r.u32()?;
        let payload=r.u64()?;let reserved=r.u64()?;
        if count==0 || count>256 || mappings==0 || mappings>512 || command_bytes>0x10000
            || output_bytes==0 || output_bytes>16*1024*1024 || output_bytes%4!=0
            || payload!=r.0.len() as u64 || reserved!=0 {return Err(EINVAL);}
        dev_info!(dev.as_ref(),"M5 render input header: {} objects, {} mappings, {} command bytes\n",count,mappings,command_bytes);
        let mut objects=KVec::with_capacity(count,GFP_KERNEL)?;let mut total=0usize;
        for _ in 0..count {
            let size=usize::try_from(r.u64()?)?;
            total=total.checked_add(size).ok_or(EOVERFLOW)?;
            if size==0 || size%mmu::UAT_PGSZ!=0 || total>128*1024*1024 {return Err(EINVAL);}
            let bytes=r.take(size)?;
            let mut object=KernelObject::backing(dev,size,CpuMap::WriteCombined)?;
            object.bytes_mut().copy_from_slice(bytes);objects.push(object,GFP_KERNEL)?;
        }
        dev_info!(dev.as_ref(),"M5 render input backing ready: {} bytes\n",total);
        let mut aliases=KVec::with_capacity(mappings,GFP_KERNEL)?;let mut pixel=None;
        for index in 0..mappings {
            let object=r.u32()? as usize;let flags=r.u32()?;let va=r.u64()?;
            let offset=usize::try_from(r.u64()?)?;let size=usize::try_from(r.u64()?)?;
            let end=offset.checked_add(size).ok_or(EOVERFLOW)?;
            let high=va.checked_add(size as u64).ok_or(EOVERFLOW)?;
            if object>=objects.len() || size==0 || end>objects[object].size()
                || (va|offset as u64|size as u64)&(mmu::UAT_PGSZ as u64-1)!=0 || flags>1 {return Err(EINVAL);}
            let prot=if flags==1 {mmu::PROT_GPU_SHARED_RO}else{mmu::PROT_GPU_SHARED_RW};
            let alias=objects[object].map_range(vm,offset..end,va..high,mmu::UAT_PGSZ as u64,prot,0).inspect_err(|e| {
                dev_err!(dev.as_ref(),"M5 render input map {} {:#x}+{:#x}: {:?}\n",index,va,size,e);
            })?;
            if alias.iova()!=va {return Err(EIO);}
            aliases.push(alias,GFP_KERNEL)?;
            if va<=output && output.checked_add(output_bytes as u64).is_some_and(|e|e<=high) {
                if pixel.is_some() || flags!=0 {return Err(EINVAL);}
                pixel=Some((object,offset+usize::try_from(output-va)?));
            }
        }
        dev_info!(dev.as_ref(),"M5 render input mappings ready\n");
        let commands=r.take(command_bytes)?;
        if !r.0.is_empty(){return Err(EINVAL);}
        let window=command::UscWindow::new(usc,range)?;
        let mut parser=command::Parser::new(commands);
        let command=parser.next().inspect_err(|e| {dev_err!(dev.as_ref(),"M5 render input parser: {:?}\n",e);})?.ok_or(EINVAL)?;
        let validated=command.payload.validate(&window,&Space(vm), super::command::M5_MAX_TILE_BUFFER).inspect_err(|e| {dev_err!(dev.as_ref(),"M5 render input validation: {:?}\n",e);})?;
        if parser.next()?.is_some(){return Err(EINVAL);}
        parser.finish()?;
        let command::Validated::Render{pass,attachments,..}=validated else{return Err(EINVAL)};
        if !attachments[1].as_slice().iter().any(|a|a.va==output&&a.size==output_bytes as u64){return Err(EINVAL);}
        let (pixel_object,pixel_offset)=pixel.ok_or(EINVAL)?;
        for i in 0..output_bytes/4 {objects[pixel_object].word(pixel_offset+i*4)?.store(0xa5a5a5a5,Ordering::Relaxed);}
        dev_info!(dev.as_ref(),"M5 render input validated: {}x{}, output {:#x}\n",pass.width,pass.height,output);
        Ok(Self{aliases,objects,pass,pixel_object,pixel_offset,pixel_words:output_bytes/4,expected})
    }
    pub(super) fn reset_result(&self)->Result {
        for i in 0..self.pixel_words {
            self.objects[self.pixel_object].word(self.pixel_offset+i*4)?.store(0xa5a5a5a5,Ordering::Relaxed);
        }
        Ok(())
    }
    pub(super) fn pixel_words(&self)->usize {self.pixel_words}
    pub(super) fn result(&self)->Result<(usize,usize,u32)>{
        let mut wrong=0;let mut untouched=0;let mut first=0;
        for i in 0..self.pixel_words {
            let value=self.objects[self.pixel_object].word(self.pixel_offset+i*4)?.load(Ordering::Acquire);
            if i==0{first=value;}
            wrong+=usize::from(value!=self.expected);untouched+=usize::from(value==0xa5a5a5a5);
        }
        Ok((wrong,untouched,first))
    }
}
