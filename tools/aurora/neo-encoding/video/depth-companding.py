#!/usr/bin/env python3
"""Reconstruct Apple's depth wrapper LUT from the inspected framework algorithm.

All arithmetic stages round to binary32; each LUT threshold is binary16.
This is a pixel conversion helper, not an encoder or a hardware success claim.
"""
from array import array
import ctypes
import math
import struct

def f32(x): return struct.unpack('<f',struct.pack('<f',x))[0]
def half_bits(x): return struct.unpack('<H',struct.pack('<e',x))[0]
def half_value(x): return struct.unpack('<e',struct.pack('<H',x))[0]

_powf=ctypes.CDLL(None).powf
_powf.argtypes=[ctypes.c_float,ctypes.c_float]
_powf.restype=ctypes.c_float
STEP=struct.unpack('<f',bytes.fromhex('0820803a'))[0]  # binary32 1/1023

def limits(kind, decoding=False):
    if kind not in ('depth','disparity'): raise ValueError('Unsupported auxiliary format')
    low,high=(f32(.2),10.) if kind=='depth' else (0.,16.)
    if decoding:low,high=(math.floor(x*65536)/65536 for x in (low,high))
    return f32(low),f32(high)

def tables(low,high):
    low,high=f32(low),f32(high)
    if not (math.isfinite(low) and math.isfinite(high) and 0<=low<high<=65504):
        raise ValueError('Invalid finite half-float companding range')
    span=f32(f32(high-low)+1)
    ratio=_powf(span,STEP)
    encoder=array('H',[0])*65536
    accumulator=_powf(span,f32(-STEP/2)); previous=0
    for code in range(1024):
        accumulator=f32(ratio*accumulator)
        threshold=half_bits(f32(low+f32(accumulator-1)))
        if not previous<=threshold<32768:raise ValueError('Nonmonotonic half-float thresholds')
        encoder[previous:threshold]=array('H',[code<<6])*(threshold-previous)
        previous=threshold
    encoder[previous:32768]=array('H',[1023<<6])*(32768-previous)
    decoder=array('f');accumulator=_powf(span,-STEP)
    for code in range(1024):
        accumulator=f32(ratio*accumulator)
        decoder.append(half_value(half_bits(f32(low+f32(accumulator-1)))))
    return encoder,decoder

def encode_f32(raw,kind):
    if len(raw)%4:raise ValueError('Truncated float32 input')
    source=array('f',raw);lut,_=tables(*limits(kind));encoded=array('H')
    for value in source:
        if not math.isfinite(value) or not 0<=value<=65504:
            raise ValueError('Qualified depth input requires finite nonnegative half-float range')
        encoded.append(lut[half_bits(value)])
    return encoded.tobytes()

def decode_codes(raw,kind):
    if len(raw)%2:raise ValueError('Truncated monochrome input')
    codes=array('H',raw);_,lut=tables(*limits(kind,decoding=True))
    if any(v>1023 for v in codes):raise ValueError('Require low-aligned gray10 values')
    return array('f',(lut[v] for v in codes)).tobytes()
