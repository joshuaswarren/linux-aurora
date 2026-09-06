// SPDX-License-Identifier: GPL-2.0-only OR MIT
#![recursion_limit = "2048"]

//! Driver for the Apple AGX GPUs found in Apple Silicon SoCs.

mod agx_power_recovery;
mod alloc;
mod apple_gpu_topology;
mod buffer;
mod channel;
#[cfg(CONFIG_DEV_COREDUMP)]
mod crashdump;
mod debug;
mod driver;
mod drm_gpu;
mod event;
mod file;
mod float;
mod fw;
mod g15_boot;
mod g15_initdata;
mod g17_adt_j700;
mod g17_asc_bringup;
mod g17_boot;
mod g17_completion;
mod g17_compute;
mod g17_drm;
mod g17_firmware;
mod g17_initdata;
mod g17_job;
mod g17_status;
mod g17_queue_limits;
mod g17_lifecycle;
mod g17_live_boot;
mod g17_manager;
mod g17_power_preflight;
mod g17_render;
mod g17_resources;
mod g17_rtkit;
mod g17_submission;
mod g17_trace_capture;
mod g17_uapi;
mod gem;
mod gpu;
mod hw;
mod identity;
mod initdata;
mod mem;
mod microseq;
mod mmu;
mod object;
mod pgtable;
mod queue;
mod regs;
mod slotalloc;
mod uat;
mod util;
#[cfg(CONFIG_DRM_ASAHI_MAPLE_TREE)]
mod vm;
mod workqueue;

kernel::module_platform_driver! {
    type: driver::AsahiDriver,
    name: "asahi",
    description: "AGX GPU driver for Apple silicon SoCs",
    license: "Dual MIT/GPL",
    params: {
        debug_flags: u64 {
            default: 0,
            // permissions: 0o644,
            description: "Debug flags",
        },
        fault_control: u32 {
            default: 0xb,
            // permissions: 0,
            description: "Fault control (0x0: hard faults, 0xb: macOS default)",
        },
        initial_tvb_size: usize {
            default: 0x8,
            // permissions: 0o644,
            description: "Initial TVB size in blocks",
        },
        robust_isolation: u32 {
            default: 0,
            // permissions: 0o644,
            description: "Fully isolate GPU contexts (limits performance)",
        },
        g17p_simplefb_iova: u64 {
            default: 0,
            description: "DIAGNOSTIC. Non-zero maps the exact active apple,simple-framebuffer boot aperture into every T8140 user VM at this GPU VA, uncached and read/write, so a bounded harness can make GPU stores visible without a DCP driver. The DT node, x8r8g8b8 geometry, no-map framebuffer reservation, page alignment and complete VA interval are all validated before any mapping; the alias is retained until VM destruction and excluded from ordinary user unbinds. This does not touch DCP or copy pixels. Use only while simpledrm owns the iBoot framebuffer. 0: off (default).",
        },
        g17p_reg_dump: u32 {
            default: 0,
            description: "DIAGNOSTIC. Dump 0x100 bytes of the sgx register window at (value << 8), after the render doorbells, as 16 rows of four u32. For the pre-firmware hardware-state audit: the SKSM submit protocol, the initdata and the kext's one-time bring-up have all been shown byte-exact or equivalent to native, so the remaining variable is the GPU register state m1n1 and this driver leave behind, which initdata does not describe. Read-only, and placed in the post-doorbell window where the cores are powered -- an sgx read with the cores gated raises an asynchronous SError and panics the machine, which is why this is opt-in and 0x100-granular rather than an arbitrary address. Example: 0xe010 dumps the GPC block at 0xe01000. 0: off (default).",
        },
        g17p_scan_gate: u32 {
            default: 0,
            description: "DIAGNOSTIC. After the render doorbells, read the two firmware words that gate b1's DM work-ring scan FUN_261a4 at 0x263b8..0x263dc: the run-state word 0x1771f8 (bits 0x18 and 0x60) and DAT_124ba4. The gate is (DAT_1771f8 & 0x18)==0 && ((DAT_1771f8 & 0x60)==0 || DAT_124ba4==1). This matters because FUN_261a4 advances the read index and sets coherency=write on ALL twelve rings on every pass regardless of contents, so our render tables 6/7 staying at {0,0,1} after a doorbell means the scan never ran -- a rejected entry would still have had its read index advanced. If these bits are set, the firmware is skipping the work scan and that is the blocker. Read-only; the driver never writes either word. 0: off (default), 1: sample 64 times at each of the two addresses.",
        },
        g17p_test_stage: u32 {
            default: 2,
            // permissions: 0,
            description: "G17P bring-up stage (0: control, 1: QID4, 2: compute UAPI; default: full driver path)",
        },
        g17p_max_recoveries: u32 {

            default: 0,

            description: "Max firmware recoveries serviced per session (0: unlimited). Diagnostic for the J700 recovery loop.",

        },
        g17p_native_order: u32 {
            default: 1,
            description: "G17P command publication order (1: kext order tag3,15,14,16 -- the default; 0: legacy tag15,16,14)",
        },
        g17p_submit_polls: u32 {
            default: 500,
            description: "G17P first-submit completion polls, 1ms each (default 500)",
        },
        g17p_probe_low_range: u32 {
            default: 0,
            description: "Probe sgx low register range mid-submit while the GPU is powered (0: off)",
        },
        g17p_late_sksm: u32 {
            default: 0,
            description: "Replay suppressed SKSM port writes once the GPU cores are powered (0: off)",
        },
        g17p_fw_log: u32 {
            default: 0,
            description: "Enable the GPU firmware log ring (main_config+0x250)",
        },
        g17p_fw_trace: u32 {
            default: 0,
            description: "Optional firmware trace class mask at status_a+0x00 on both roles. Disabled by default for normal asynchronous execution: the trace rings require their own consumer and must not backpressure GPU completion. Nonzero is an explicit diagnostic mode; submission/progress classes are0/1/9.",
        },
        g17p_fw_ktrace_verbose: u32 {
            default: 1,
            description: "Print every KTrace record instead of only the submission-path codes (0x41/0x46/0x4b/0x57/0x0a/0x0c/0x0e/0x20 and the 0x11c lost marker). Default 1 while G17P graphics bring-up needs the otherwise-hidden 0x1c/0x1f/0x60 admission-path witnesses.",
        },
        g17p_fw_ktrace_budget: u32 {
            default: 32,
            description: "Maximum other verbose KTrace records printed per drain pass (0: unlimited). Submission class0 and native monitor keys each have an independent64-record allowance and explicit drop counts. Records over every print budget are still consumed and acknowledged.",
        },
        g17p_flushid_fix: u32 {
            default: 0,
            description: "Tag-15 FlushID fields: 0 keeps ours (shared_control/0x3f/1), 1 uses the kext constants (0/-1/-1)",
        },
        g17p_skip: u32 {
            default: 0,
            description: "Bisect the submission: bit0 skip tag-14 AddKicks, bit1 skip tag-16 EntrySignal, bit2 skip EP21 doorbells",
        },
        g17p_defer_add_kicks: u32 {
            default: 0,
            description: "Publish tag-14 AddKicks only after the firmware recovery/power-up closes (1: native behaviour)",
        },
        g17p_clear_fault: u32 {
            default: 1,
            description: "Acknowledge any stale GMMU fault before booting the firmware (1: on)",
        },
        g17p_ctx2_root: u32 {
            default: 3,
            description: "Bitmask of the compute kick's GPU hardware contexts to publish translation roots for: bit0 = context 2 (workload 0), bit1 = context 3 (m1n1's retained pair, every later workload). Each context is aliased onto the bound compute VM's low root plus the populated firmware high root, tagged with the context as its ASID, exactly as m1n1's hardware-proven agx_g17p_native_add3.py set_l0(CONTEXT,0,root,CONTEXT)/set_l0(CONTEXT,1,ttbr1_base,CONTEXT) does. Default 3. 0: previous behaviour, those contexts have no root at all and the first gUPM fetch takes a Page Invalid fault with vm_slot=2.",
        },
        // NOTE: every `permissions:` line in this block is commented out, so
        // the macro gives each parameter permissions 0. That means NONE of
        // these appear under /sys/module/asahi/parameters -- they can only be
        // set on the insmod command line. Looking for one in sysfs and not
        // finding it does not mean the build lacks it; check
        // `strings asahi.ko | grep parmtype=` instead. This cost a hardware
        // cycle once already.
        g17p_vm_probe_va: u64 {
            default: 0,
            description: "Non-zero: at every compute submit, hexdump 64 bytes of GPU-visible memory at this VA and probe every 8-byte word inside it that looks like a user VA, dumping 32 bytes at each. Chases the descriptor chain the SHADER walks -- root descriptor -> sets[0] -> the buffer address it stores through -- none of which appears in the UAPI or in the command stream, so nothing else can name it. Set it to Mesa's `G17P root: va=` value (HK_G17P_LAUNCH_TRACE=1). 0: off.",
        },
        g17p_vdm_dump: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- hexdump the first 128 bytes of Mesa's VDM control stream at render submit. The tiler demonstrably fetches this stream (pipe-vdm-stream reads back the encoded pointer on pipes 0 and 2 with work stamps 0x100) and then bins essentially nothing: 60 non-zero bytes in a 1277952-byte tile buffer, and the content is an initialised-but-empty bounding box carrying 0x7fc00000 (NaN) and 0x7fffffff extremes. Either the stream contains no draw, or it draws primitives that are all culled. This shows which. 0: off (default), 1: dump.",
        },
        g17p_fault_report: u32 {
            // Default 0, for the same reason as g17p_render_ksm_probe: it
            // performs sgx MMIO on the submit path, and native performs none.
            default: 0,
            description: "Log the firmware fault report reached through main_config+0x26c (blamed qid, data master, recovery state) next to the MMU fault bank (1: on)",
        },
        g17p_progress_watchdog: u32 {
            default: 1,
            description: "KSM progress watchdog threshold at regionC+0x9cc (1: keep native 0x32, 0: disable)",
        },
        g17p_power_hold: u32 {
            default: 0,
            description: "After IdlePowerOff(0) powers the cores, publish a second device-control record with arg=1 to inhibit the firmware's idle power-down for the duration of the submit (1: on)",
        },
        g17p_power_wire: u32 {
            default: 0,
            description: "Publish a device-control IdlePowerOff(0) record (opcode 0x0a, arg 0) and announce it on EP 0x21. It reaches 0xfffffc0000021960, which is the only routine that releases KSM pause reason bit 1. 0: off (default, and what native does -- a live dtrace capture of a Metal dispatch shows no device-control record at all). 1: publish it in the submit PREAMBLE, before the kick. 2: publish it AFTER the kick and its doorbell. Mode 2 exists because firmware RE of b1 shows the ORDER is load-bearing whenever the pause mask has more than one bit set: every clear site -- bit 1 at 0xe488, bit 3 at 0x25dd4 -- is an `ands` followed by `b.ne <skip>`, so the core power sequencer 0x20a70 runs ONLY on the transition to mask == 0. With the post-fault mask of 0xa (bits 3|1) a preamble clear of bit 1 leaves 0x8 and is skipped by construction, which is why mode 1 measured as a no-op. Deferring lets the scheduler clear bit 3 first (0xdeb8 reports outstanding work -> 0x25c00 -> 0x25dc0) so that this record's bit-1 clear is the one that reaches zero. NOTE 0x21960 is itself double-gated: it early-exits at 0x21980 when [0xfffffc00001771f8] bit 1 says the ASC has napped.",
        },
        g17p_idle_selector: u32 {
            default: 0,
            description: "Diagnostic override for regionC+0xe21 idle-descent selector. Native initializes 0; non-zero forces the alternate firmware predicate. 0: native default, 1: override.",
        },
        g17p_render_absolute_pointers: u32 {
            default: 0,
            description: "Diagnostic escape hatch for G17P VDM register 0x1c880. Native lowers a task VA to a task-relative offset, maps the same backing into the architectural GART window at 0x1000000000 + offset, and writes the offset; live fault words prove hardware adds 0x1000000000 back when compact-pointer bit 39 is clear, while bit 39 selects the upper UAT root. Linux userspace supplies the validated full GART-alias VA. 0 (default): subtract 0x1000000000 and reject offsets >= 1<<39. 1: write the full VA directly, reproducing the upper-root dVDM_CSD fault. Tilemap/TPC/heapmeta remain relative to the separate driver-private render-state context in both modes.",
        },
        g17p_render_status_flag: u32 {
            default: 0,
            description: "Seed word 0 of the render status page(s) with 1, matching what the driver already writes into the compute runtime state. MEASURED BASIS: the page at 0xfffffc2001630000 reads word0=1 on a compute submit that completes 64/64 exact, and word0=0 on every render submit, sampled at the same points. That VA is declared to the firmware by initdata (g17_initdata.rs:838, put_u64(out, 0x4c, COMPUTE_RUNTIME_STATE_ADDRESS)) and G17P_BOOTSTRAP_3D_STATUS_VA is bit-for-bit the same address, so on a render-only boot the firmware's declared runtime-state page is backed by render's zeroed 3D status page. The driver seeds the compute object's word 0 with 1 in three places (g17_resources.rs ~5278, ~7374, ~7859); render has no equivalent. 0: off (default), 1: seed the 3D status page only -- the one that shares the VA, 2: seed the TA status page as well. NOTE the 1 is written by the DRIVER, not the firmware, so this tests whether the firmware requires that flag, not whether the firmware sets it.",
        },
        g17p_render_gate_probe: u32 {
            default: 0,
            description: "Sample the scheduler-gate registers (gpc-state, fender dynamic gating, host IRQ enable/status) in a burst immediately after the render doorbell, logged as checkpoint render-post-doorbell. This is the observable that actually moves on a submission which executes: a working compute submit reports gpc-state 0x0 -> 0x1 -> 0x0, while scan-active reads 0 on that same run and is therefore meaningless. The render path had no equivalent sample at all. UNSAFE BY CONSTRUCTION, which is why it defaults off: the read touches the sgx register file, and an sgx read with the GPU cores power-gated raises an asynchronous SError and panics the machine. It is taken ONLY in the window right after the doorbell, never before publish (cores may still be gated -- render-pre-doorbell is DRAM-only for that reason) and never at the wait timeout (cores have powered back down; this panicked the box every time it was tried on the compute path). Compute's own sample is safe only because it runs after a completion woke the caller. 0: off (default), 1: 12 samples at 1ms, 2: 40 samples at 1ms for a wider window.",
        },
        g17p_render_ksm_probe: u32 {
            // Default 0. MEASURED on macOS 25G83 under the m1n1 hypervisor
            // (20260904-001852Z, full desktop session, 95,726 sgx MMIO events
            // with PC attribution against the 25G83 kernelcache): the AGX
            // driver performs TWENTY-SIX register accesses in the entire
            // session -- 16 in parsePerfStateMapRegs, 7 in readChipInfo, and 3
            // on 0xd06030 -- all one-time init apart from the fender gating.
            // ZERO during any of its 10,803 render doorbells.
            //
            // This probe reads the KSM through the SKSM tag/data port, and
            // g17p_sksm_port_read_locked WRITES the tag to sgx+0x4090 before
            // reading 0x4098. Native never touches that port at all, and this
            // file's own regs.rs:517 says it "must never be loaded". Doing it
            // on every submit is a host action with no counterpart in the
            // oracle, on the path where the KSM decides whether to promote an
            // admitted queue. Diagnostics do not belong in PARAMS=''.
            default: 0,
            description: "G17P GRAPHICS BRING-UP TRACE. After both native render doorbells are sent, wait up to 40ms and take power-gated-safe snapshots of the kernel-owned TA/3D queues, SGX current-command/context, and the bit-4 new-work / bit-3 completion launch registers consumed by B1 FUN_23004. The GPC-state read is a hard gate before low SGX accesses. Default 1 while the TA-to-3D callback boundary is under bring-up; 0 disables it.",
        },
        g17p_render_header_flags: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY. Force native conditional bits in the render KSM entry header. Bit 0 sets header bit 1 on the 3D entry only; AGXKSMKickQueueG16::encode3DKickEntry passes it when host descriptor +0xc6c is 3 or +0x4c bit 0 is set. Bit 1 sets header bit 61 on both render entries; native actually sources it from the device-global [channel->device+0x6b7] & 3 mode, not descriptor memory. The captured/default device mode leaves it clear. 0 is the native default encoding.",
        },
        g17p_render_generation_bias: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY. Bias the render tag-15 generation fields +0x2a/+0x2e by one. 0 (default) keeps the native cold generation zero. The complete valid trace also proves opcode-0x20 +0x0c and tag-15 +0x3e begin at zero; forcing a first-generation replacement diverges from B1's reset state. 1 retains the historical bias experiment.",
        },
        g17p_render_bg_prefix: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY. Override the main background program resource-specifier prefix. 0 (default) preserves the UAPI value; valid cold qid1 carries 0x0007800000000040 and 308/437 valid commands use the same high prefix. 1 forces the stale single-capture 0xffff8000 prefix. Partial-background policy is distinct and must not be inferred from this switch.",
        },
        g17p_render_fragment_lifecycle: u32 {
            default: 0,
            description: "DIAGNOSTIC, native-derived ownership without captured literals. Metal writes one nonzero 64-bit fragment event correlation to registers 0x160e0/0x01499/0x0a341: low32 is the current 3D command-publication stamp and high32 is a preceding dependency event. Linux currently writes zero to all three and incorrectly shares the source field with TA, whose native correlation is separate. Mode 1 leaves TA unchanged, samples the firmware StatusB +0x4920 TDR scheduler-progress cache only as a safe Linux seed, and derives a nonzero monotonic fragment-local dependency/current pair from the retained render ordinal. That StatusB word is not established as the native event source. Firmware does not validate the captured numeric values, so none are copied. 0: current zero baseline. 1: stage-specific derived fragment correlation.",
        },
        g17p_render_eot_bind_zero: u32 {
            default: 1,
            description: "G17P fragment end-of-tile/store resource-specifier bind. 1 (default) writes zero coherently in the main, partial-store, partial-resume, and retained descriptor-tail copies; all 437 unique valid Metal 3D command captures write register 0x15379 as zero. 0 diagnostically preserves the legacy userspace ABI value (currently 8).",
        },
        g17p_render_dump_descriptor: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY. Dump the complete post-encoding 0x2240-byte G17P 3D descriptor in compact 64-byte rows before the first render publication, after retained fields, generic tails, and optional MCache mutations have all been applied. This is intended for byte-for-byte comparison against a working Metal descriptor capture without evicting launch evidence from the printk ring. 0: off. 1: dump the bootstrap descriptor.",
        },
        g17p_render_mcache_hwsid: u32 {
            default: 24,
            description: "Render MCache HWDSID. 24 (default) is the physical default selected by logical DSID 0x100 in the G17P kext and is present in both records of the valid cold class-0 3D aperture. 0 diagnostically suppresses active render MCache mappings. Values 1..255 pack validated UAPI fragment-write ranges into the matching fragment SKSM aperture; tables use descriptor +0x6a0 or owned page padding when the inline capacity is exceeded. The native class-0 shape with 24 was tested on Neo without regression. Never use with absent fragment attachments.",
        },
        g17p_render_mcache_mode: u32 {
            default: 1,
            description: "Render MCache range topology. 1 (default) emits driver-owned aux-FB with shift 2, then every validated fragment attachment with shift 1, preserving the qualified two-range shape for one attachment. 0 emits one record per UAPI attachment. 2 requires exactly two validated UAPI ranges and emits them unchanged in native order with shifts 2/1. Every entry shares one HWDSID/selector; fragment KSM index is 0 and count covers the complete table. No captured native VA is copied. Requires non-zero g17p_render_mcache_hwsid.",
        },
        g17p_render_mcache_selector: u32 {
            default: 64,
            description: "DIAGNOSTIC ONLY. MCache write-range selector shared by every emitted record. Values 0..63 are encoded directly; 64 (default) uses the currently bound render root slot. Working Metal witnesses use AGXContextIDManager-derived selectors 2 or 3 even when descriptor context is 1. For those two witnessed values Linux now mirrors the bound render GART roots into the matching TTBAT slot before making the first outer producer visible, reproducing AGXSecureGart::registerContextID instead of emitting an unbacked selector. Values above 64 reject the render.",
        },
        g17p_render_ksm_mcache: u32 {
            default: 1,
            description: "Publish the fragment descriptor MCache table as a KSM entry aperture. 1 (default) matches the valid cold class-0 qid1 entry: header bit58 set and +0x08 naming descriptor +0x6a0, index 0, count 2. 0 suppresses the KSM aperture for later native priority-2 profiles whose host command count is zero. The native class-0 shape was tested on Neo without regression. The TA entry never receives an aperture.",
        },
        g17p_render_no_barrier: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- delete the real TA timestamp dependency from the 3D KSM entry. Kext RE corrected the earlier pool theory: the KSM dependency contains only timestamp+QID, never a barrier-pool slot or pointer. Native does reserve temporary vendevtBarrierRec storage while building the event list, but setupBarrierCommand compares source ownership against the target context ID and deliberately skips publishing tag-4 for this captured same-context TA-to-3D edge; completion releases the temporary host record regardless. Therefore Linux's one explicit dependency is the native payload and there is no dangling unowned slot. 0: correct dependency present (default). 1: historical suppression diagnostic only; not a fix.",
        },
        g17p_render_split_launch: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- make the paired render submission a true whole-group split. Before the first channel-0 doorbell the 3D command/tag-15 bytes may be staged in host memory, but its SKSM entry, tag-14 AddKicks/item producer, and outer data-master producer all remain unpublished; only the complete TA group is visible. After both TA-owned timestamps become non-zero, the driver publishes the 3D SKSM entry/dependency, then tag-14 and its inner producer, then the retained 3D outer slot/producer, and sends the same channel-0 doorbell again. This isolates 3D descriptor execution from simultaneous paired-launch/dependency chronology without ever publishing and rewinding live state. 0: normal fragment-first paired publication (default, byte-for-byte unchanged), 1: TA completion then complete 3D group publication.",
        },
        g17p_render_native_doorbells: u32 {
            default: 0,
            description: "Mac17P render TX chronology. 0 (default): initial-bring-up profile -- qids 0/1 on TA_0/3D_0, publish the complete pair and send the single paired Vertex/priority-0 doorbell captured before native creates userspace priority-2 queues. 1: paired Fragment then TA diagnostic. 2: later userspace fragment-first chronology. 3: mode 2 plus a diagnostic bounded pre-ack. Modes 1/2/3 are mutually exclusive with split launch; modes 2/3 require render SKSM.",
        },
        g17p_render_native_pool_a_first_record: u32 {
            default: 1,
            description: "Use initialized Pool-A record 1 for the cold first render pass, matching the native G17P publication: TA descriptor +0x10 and 3D descriptor +0x20 both advance from the Pool-A base by 0x100, record 1 carries its matching slot pointer and +0x10 state 0x50, slot 1 is seeded to 1 before 3D publication, and late TA publication advances that same slot to 2. Value 0 retains the obsolete record-0 diagnostic. Retained ordinal*2 selection is unchanged because native scenes can reuse an index across multiple sequence values. No tag16, queue, RCE, SKSM, or doorbell field changes.",
        },
        g17p_render_native_fragment_hwpb: u32 {
            default: 1,
            description: "G17P parameter-buffer binding mode. 0: established packed-object fallback. 1: bind the exact PM-mode2 0xc0-byte HWPB state at both TA +0x20 and Fragment +0x28 and publish the same object in TA ConfigUpdate +0x6e, matching Metal pointer identity. 2: DIAGNOSTIC ONLY -- keep the mode-1 descriptor pointers and coherent PBDesc/HWPB ID but publish zero at TA ConfigUpdate +0x6e, isolating firmware HWPB-bind admission from descriptor execution. 3: DIAGNOSTIC ONLY -- reproduce the last TA-starting Linux pointer arrangement: packed object at TA +0x20 and TA ConfigUpdate +0x6e, PM HWPB only at Fragment +0x28.",
        },
        g17p_render_3d_rce: u32 {
            default: 0xff,
            description: "DIAGNOSTIC ONLY -- number of RegisterArray (RCE) bindings published in the fragment's kick entry. 0xff (default) keeps the native four, at descriptor+0xa0 on the measured 0x720 stride tagged 0x56/0x10/0x17/0x0a. Under g17p_render_sksm=3 the TILER, which carries a single binding, is dispatched classically and executes, while the FRAGMENT, which carries four, is admitted to a KSM slot and sticks at phase 6. Modes 1 and 2 put the tiler in that same stuck state, so the entry shape is what decides classic versus SKSM admission. This asked whether the binding COUNT is the deciding term. REFUTED ON HARDWARE 2026-09-02: with one binding, and with zero, the fragment is still admitted to KSM slot 0 at state 0x0100000d phase 6 and retired status 4, byte-identical to the native four. The count is not the term; the data master is. Kept default-off so the negative is not re-derived. Publishing fewer than native does is not correct rendering and is not a fix.",
        },
        g17p_seed_work_channel: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- before the render doorbell, seed the KSM work-channel word sgx+0x21168 with this value (0: off). Diffing a cold render-only boot against a compute-then-render boot shows exactly one difference in the scheduler bank at the same poll: work-channels goes [0,0,0,0] -> [1,0,0,0], and with it the tiler stops being admitted to a KSM slot (where it sticks at state 0x0100000d phase 6, exactly as the fragment does) and instead takes the classic path and executes. This asks whether that word is the gate a completed compute establishes.",
        },
        g17p_render_3d_strobe: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- issue the 3D launch strobe from the host during the render poll: write (value-1) to sgx+0x10398 then 2 to sgx+0x1c9a0. 0: off. b1's 0x1c9a0 = 2 is the only 3D launch in the image and its single writer is the tiling job's state 4, which is unreachable here because the new-work interrupt is never raised and no firmware job is ever built. The hardware has latched the fragment at 0xc0f0/0xc0f8 with dm-masks[1] set, so the work is presented and only the strobe is missing. Native performs zero MMIO on the submit path, so this can never ship; it answers whether the strobe alone starts the fragment.",
        },
        g17p_irq_burst: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- after the render doorbell, burst-sample the GPU interrupt status 0x1_0a10 with the new-work mask 0x1_0a60 and completion mask 0x1_0a50, 512 times, and log the distinct triples. Both bit 3 (tiling done) and bit 4 (new work) are enabled in 0x1_0a08 = 0xa00620038 and have never been observed set at poll granularity. If they never appear in a tight burst either, the firmware is not receiving GPU hardware interrupts at all, which would explain why no job is created for ANY workload -- compute and the tiler included -- and why the 3D strobe, reachable only from a job, never happens. Read-only.",
        },
        g17p_slot_burst: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- immediately after the render doorbell, sample KSM slot 0's state register 256 times and log the distinct values in order. The poll loop only ever sees the slot empty and then the fragment stuck at 0x0100000d a millisecond later, so no sample has ever caught a queue while it is actually dispatched and progressing. This is read-only through the SKSM port.",
        },
        g17p_dump_state: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- dump this many KiB of the primary firmware-visible state object once, at the first render checkpoint. A completed compute is what makes the tiler run; registers have been compared cold-versus-warm and the only difference, work-channels[0] going 0 to 1, was refuted by seeding it. The firmware-visible DRAM has never been compared, and whatever a compute changes there is the candidate set for the bootstrap. Very noisy; capture via /dev/kmsg, not dmesg, since the ring buffer wraps.",
        },
        g17p_dump_hwdata: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- dump the whole 0x3db4 hardware-data block this driver builds, once, at the first render checkpoint. HW_DATA_OPAQUE_RUNS (the 269 hardcoded bytes) is already verified byte-exact against native; the rest of the block is computed and has never been compared. Native's block was captured whole from AGXArmFirmwareASC + 0x2b8, so this makes a full diff possible. Very noisy; off by default.",
        },
        g17p_pipe_sweep: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- sweep the pipe register window 0x1c800..0x1cc00 for both pipes during the render poll and log every offset that differs or is non-zero. This is 256 SKSM tag/data port reads on the submit path and it destabilises the machine, so it is off by default and must never be enabled in a canonical run. It already produced its result: pipe 1's register file is present and configured (0x1c800, 0x1c820, 0x1c890, 0x1c8e0/e8, 0x1c990, 0x1c9f8/0x1ca00, 0x1cbe0 all read identically on both pipes), pipe 0 alone carries the render's buffers (0x1c9c8=0x02018000 tilemap, 0x1c9d8=0x02019000 heapmeta, 0x1c880 control stream, 0x1c9f0 work stamp), and the one flag-shaped asymmetry is 0x1cbd8, where pipe0=0xe0000007 and pipe1=0x60000007 -- identical but for bit 31.",
        },
        g17p_render_ta_dm: u32 {
            default: 0xff,
            description: "DIAGNOSTIC ONLY -- override the data master the TILING queue is published on. 0xff (default) keeps the native G17P_TA_DATA_MASTER. Companion to g17p_render_3d_dm: moving the fragment from DM1 to DM0 makes it execute, so this asks whether data master 1 is dead for ANY stage or only mis-configured for the fragment. Not a fix.",
        },
        g17p_render_3d_dm: u32 {
            default: 0xff,
            description: "DIAGNOSTIC ONLY -- override the data master the fragment queue is published on. 0xff (default) keeps the native value G17P_3D_DATA_MASTER. The tiler (DM0) and compute (DM2) both dispatch and execute; the fragment (DM1) is retired with KTrace 0x43 status 4 without ever running, with an identically shaped descriptor, a satisfied barrier, and geometry already tiled into the TVB. This asks the one question that isolates the data master itself: does ANY queue on DM1 dispatch on this firmware? Rendering with the fragment on DM0 is not correct and this is not a candidate fix -- it is a discriminator.",
        },
        g17p_fw_trace_classes: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- arm the firmware KTrace CLASS bytes at 0x17780c/0x17780d before the render doorbell, so b1 emits the halt-decision records it has always suppressed. Only bit 0 of 0x17780c has ever been set on this driver, which is why every capture so far shows the submission-path codes and none of the scheduler's own reasoning. Setting bit 1 and bit 2 of 0x17780c and bit 1 of 0x17780d turns on 0x909 [pending, #state1, #state2, count | idle<<16] at 0x5aac -- the EXACT inputs to the halt decision in fn 0x5450 -- plus 0x113 [wake-bits, halt+4, halt, ...] at the thread-4 wake (0x26d70), 0x105 at the timer-thread entry (0x23f98) and 0x110 [class, unit, threshold] on the hang path (0x6c08). This matters because the cold render does not lose to a scheduler that refuses it: the firmware halts the GPU and runs a recovery/restore cycle, and a cold kick loses the race to that halt. These records say why the halt was decided. The bytes are debug-enable flags the firmware only reads; they steer nothing. 0: off (default), 1: arm on the primary ASC.",
        },
        g17p_ksm_cold_arm: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- before the render doorbell, perform from the host the one-time KSM arm that b1's scheduler pass FUN_25644 performs for itself (sgx+0x21140 = 1<<63, gated in firmware on DAT_124b8d && DAT_177d18). A cold-boot render publishes tag-15/tag-14 for both stages and is never drained; 21ms later firmware thread 4 raises pause reason 0x1000 and both queues are NOP-retired as stopped-with-pending-kicks. One completed compute makes the same publication drain 18us later and the TA execute. This asks whether that arm is the state the compute establishes. Native performs ZERO MMIO on the submit path, so a positive result identifies the gate, it does not license keeping this write.",
        },
        g17p_admit_arm: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- before the render doorbell, set bit 63 of each render queue's descriptor register 0x21008 through the SKSM port. b1's admit block at 0x12dc0 tests exactly this bit: set, the admitted byte A becomes 0x21020-0x21028+1; clear, A stays 0, which makes A == B at 0x58010+qid*0x28+0x418/0x419 and the scheduler parks the queue and NOP-retires it status 4 -- the cold-render symptom, byte for byte. Nothing in the firmware writes bit 63 (only bit 62 at 0xd1d4/0x164e4), so it is hardware-set and a cold boot is the case where the hardware never set it. Native performs ZERO MMIO on the submit path, so this can never ship; it answers whether that one bit is the cold gate. 0: off (default), 1: arm the TA queue, 2: arm both render queues.",
        },
        g17p_render_no_install: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- publish tag-15 with the queue-install flag (+0x1a) CLEAR for the render stages, so the firmware never runs its install block for qid 5/6. MEASURED 2026-09-02: on a COLD render-only boot both render queues end up fully installed (qstate +0=1 with a real entry-ring pointer, dm-active dm0=0x20/dm1=0x40, ready-mask 0x60) and the tiler is PARKED; on a WARM boot the same checkpoint shows them NOT installed (qstate +0=0, ready-mask 0x10, compute only) and the tiler EXECUTES. A queue that is not installed on the KSM bridge can therefore still run, which says the classic dispatch path does not need the install -- and raises the possibility that the install is what captures the queue for the SKSM scheduler and parks it. This asks that question directly. 0: off (default). 1: suppress the install for the TILING stage only. 2: suppress it for both render stages. Not a fix: a queue that is never installed cannot be scheduled by the KSM at all, so a positive result identifies the mechanism, it does not license shipping this.",
        },
        g17p_frag_oracle: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- bitmask, write native's value into fragment-descriptor fields where ours differs STRUCTURALLY (not merely a different allocation address). Sourced from a byte diff of our descriptor against native's 3D command record, after confirming the record stride is 0x2240 so the comparison covers the record and not its successor. The fragment executes on DM0 with real GPU timestamps and writes NO pixels, so the missing store is expected to be one of these. 1: +0x158 = 0x7007 (we never write it). 2: +0x488 = 1 (we never write it). 4: set bit 63 of +0x21c8 (native 0x8000005300000000 vs our 0x0000005300000000). 8: +0x78 = 0x40 (ours 1). 16: +0x458 = 3 and +0x470 = 0x500 (ours 1 and 0x100). 32: +0x1f48 = 0x0000010000000100 (ours 0x0000002000000020). 64: zero +0x1d0/+0x1f0/+0x1f8, which native leaves zero and we populate. 128: +0x1dc = 1.0f. Combine freely to bisect. None of this is a fix -- it identifies which field the blank render target depends on.",
        },
        g17p_ta_oracle: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- bitmask, write native's value into TA-descriptor fields where ours differs structurally at MATCHED 256x256 geometry. This is the first VALID TA comparison: earlier ones diffed a 32x32 render against native's 256x256 capture, so workload-dependent fields looked like bugs. 1: the three sites carrying 0x100 where native carries 0x500 (+0x370, +0x388, +0x8d0; the fragment descriptor carries the same pair at +0x470). 2: +0x018 = 3, which we never write at all. 4: register 0xd419's value 0x60 -> 0xa9. 8: +0x7d8 0x200 -> 6 and +0x798 6 -> 0xa. The tiler bins ~60 bytes of a 1.28 MB tile buffer while every input -- vertex shader, uniform ranges, root descriptor, vertex data, VDM stream and stream pointer -- is verified correct on hardware, so these are what remains that we demonstrably send differently. Not a fix; it identifies which field the empty tile buffer depends on.",
        },
        g17p_render_usc_freelist: u32 {
            default: 1,
            description: "NATIVE-DERIVED FIRST-SUBMIT CONTROL. Before the first render doorbell, publish AGXUSCPrivMemFList::updateSubmitInfo as the exact 0x40-byte opcode-0x20 device-control record naming Linux-owned resources. A matching firmware type-13 record is accepted only while the retained owner is live; release uses opcode 0x2e. 0: diagnostic suppression, 1: publish the cold render FList (default).",
        },
        g17p_recovery_allow_active_scheduler: u32 {
            default: 1,
            description: "Let a firmware recovery be serviced while render scheduler state is live. validate_empty_primary_firmware_recovery_scheduler refuses with EBUSY whenever render_storage, render_tracker or render_bind exists, or render_submission_ordinal is non-zero. That precondition is UNSATISFIABLE for a render-blamed recovery: the firmware raises the type-4 restart request DURING the render wait, so those fields are populated by construction at exactly the moment the handshake must be answered. Measured with g17p_render_service_events=1 -- the drain now reaches the recovery, decodes it, does the generation bookkeeping, logs 'servicing', and then dies on this guard. The guard predates render being able to trigger a recovery at all; for compute the scheduler genuinely was empty. What it actually protects is the generation-0 SKSM last-submitted timestamp wipe, which would erase a live render's progress markers and make the firmware immediately request another recovery (observed on J700 as 105 identical fault-free recoveries in one 625ms window). So this option scopes the refusal to that wipe instead of the whole handshake: with it set, the acknowledge (write 2 into status_b[0x4900]) and the final clear run regardless, and the wipe is skipped while render state is live. The precedent is in the same function -- the non-empty recovery-info table used to abort there too, until it was understood that refusing to clear is strictly worse than clearing because it pins the firmware waiting for a host answer that never comes. NOTE THE SCOPE, again: this completes the recovery handshake for a render halt. It does not make a render succeed. 0: off, keep the EBUSY (default), 1: service with a live scheduler, skipping the wipe.",
        },
        g17p_render_service_events: u32 {
            default: 1,
            description: "Service firmware events between polls of the render completion wait. The render submit runs under the runtime mutex -- g17_drm.rs calls submit_translated_render on a locked guard and the wait happens inside that call -- so the workqueue item that drains the firmware event ring blocks on mutex().lock() for the entire wait. Measured: the firmware halts, posts a type-4 kAGFIFirmwareEventTypeGPURestart naming stamp slot 0, raises the RTKit notification (four seen, role=GFX1), and the ring still reads producer=1 consumer=0 pending=1 when the wait gives up. The firmware spins until the host writes 2 into status_b[0x4900]; never answering leaves KSM halted forever, which is the ETIMEDOUT. This mirrors submit_translated_compute, which polls with the mutex held and therefore calls service_firmware_events_inline from inside its own loop instead of relying on the worker; that helper also does the queue_primary_recovery(generation, ..) bookkeeping a bare drain would skip, which is the failure mode recorded at g17_live_boot.rs:620. NOTE THE SCOPE: this makes a render halt RECOVERABLE, it does not make a render succeed. The completion records were read directly from the KSM rings, not through the worker, and were genuinely empty -- so the work did not run, and nothing here makes it run. Expect the consumer to advance to 1 and the render to fail cleanly or recover-and-fail rather than wedge. 0: off (default), 1: service inline. No locking changes either way, so compute and the DRM-level compute wait are untouched.",
        },
        g17p_compute_sksm_entry: u32 {
            default: 1,
            description: "DIAGNOSTIC CONTROL on the COMPUTE path. 1 (default): normal. 0: publish everything a compute submit normally publishes -- tag 3/15/14/16, the classic item-ring CL_2 command, the doorbell, the SKSM producer advance -- but never write the SKSM kick entry BODY. This tests the control rather than the failing path. The suppressed SKSM queue-enable (g17p_sksm_mmio=0, a no-op because the port raises an SError) was dismissed as a render blocker on the grounds that compute calls the identical suppressed function and still completes 64/64. That argument only carries if compute's completion actually depends on SKSM scheduling. This driver straddles both mechanisms: compute writes an SKSM entry AND appends a classic item-ring command, and the compute throughput ceiling that was fixed earlier was an ITEM-RING WRAP -- which a purely SKSM-scheduled path could not have been gated by. So compute may well be scheduled classically with its SKSM entry vestigial, in which case the dismissal proves nothing about render, which has no classic path at all. If compute still passes with this at 0, the SKSM enable is a live render candidate again; if compute fails, the original refutation stands. Note that a live SKSM would then be reading a zeroed entry, so a hang on that outcome is expected and is itself the answer.",
        },
        g17p_report_drain: u32 {
            default: 0,
            description: "Read the firmware EVENT ring (channel-table entry 13) at the render checkpoints and log every record. This ring carries Fault / Flag / Timeout / GrowTilingBuffer / ChannelError, and until now nothing in the driver read it: decode_channel13_event and decode_firmware_report had no live caller outside a unit test, and no code outside g17_initdata.rs referenced the report ring offsets at all. Event type 7 is the firmware asking the HOST to grow the tiling parameter buffer; the legacy AGX2 path answers it (channel.rs EventMsg::GrowTVB -> BufferManager::grow -> DeviceControlMsg::GrowTVBAck), the G17P path does not, so such a request would sit unread in DRAM while the tiler waits for a reply that never arrives -- which is exactly what a render that dispatches, powers the cores, holds them for 40ms and completes nothing looks like. NOTE the pointer arithmetic, because it is easy to get wrong: a report channel-table entry is TWO (state, ring) pairs, not three states and a ring. Entry 14 proves the shape -- the working ktrace drain uses entry[0] (status_a+0x240) and entry[1] (status_a+0xa6ac0). So entry 13 = [0x40, 0x2c0, 0x80, 0x4ac0] means the event state is status_a+0x40 (consumer +0x00, producer +0x20) and the event ring is status_a+0x2c0; status_a+0x80 is entry[2], the firmware LOG ring-0 control block that dump_firmware_log walks at 0x30 stride, and it is non-zero on every boot for reasons unrelated to tiling. The drain logs that word too, labelled fwlog-write, so the two can never be confused. 0: off (default), 1: read and log without touching the consumer cursor, 2: also publish the consumer cursor as the ktrace drain does. Mode 1 is the safe first experiment: it answers whether anything is there without writing to firmware-shared state on a ring whose consumer has never moved. Nothing acts on a record in any mode.",
        },
        g17p_report_drain_budget: u32 {
            default: 0,
            description: "Maximum event-ring records logged per drain call (0: unlimited). Records over budget are still counted.",
        },
        // 2026-09-01 follow-up: descriptor +0x789=0x10 was also hardware-null
        // (same fault-free TA timeout and recovery as the 0x08 baseline).
        g17p_render_native_regs: u32 {
            default: 0,
            description: "DIAGNOSTIC captured register values retained after native provenance auditing. Active bits: bit 1 (2) replaces TA 0x0a5a1's lawful zero-source fallback 0x6000400020 with one workload-derived Metal value 0xfe00400020; bit 3 (8) similarly replaces fragment 0x0a5a9 with captured 0xd500400020; bit 5 (32) forces fragment register 0x1c838 to zero without changing the independently sourced descriptor tail. TA 0x14308 and fragment 0x14048 now both come from the retained shared USC FList HardwareBufferID. Bits 0 (1) and 4 (16) are retired; TA 0x1c830 comes from the native-shaped parameter-buffer ID allocator. Bit 2 (4) is retired because fragment 0x1c838 comes from cached SGX 0xe01480 bits 19:16; tail +0x210c comes from bits 3:0, +0x2104 from SGX 0xe0141c bit 0, and +0x2110 is a separate native render-setup field. 0: lawful Linux-owned values (default).",
        },
        g17p_render_native_bytes: u32 {
            default: 0,
            description: "MEASURED 2026-08-31: NO EFFECT. Setting this to 1 on hardware produced a run identical to baseline in every observable -- both descriptors retired into completion ordinal 0 with start=0x0, ordinal 2 empty, pause-mask 0x0, restart request serviced, same sequence. So TA descriptor bytes +0x93f (0x91 vs native 0x87) and +0x94d (0x01 vs native 0x03) DO NOT MATTER, and the two-byte divergence found by auditing our literals against recs4096-gfx.txt is a dead end. Do not regenerate this change from the same capture. Kept as a parameter so the null result stays reproducible. ORIGINAL RATIONALE: send the witnessed values for two TA descriptor bytes -- +0x93f = 0x87 and +0x94d = 0x03 -- instead of this driver's 0x91 and 0x01. A systematic audit of every literal put_u8/16/32/64 in build_ta_descriptor and build_cold_opening_ta_descriptor against native TA descriptor 0 in recs4096-gfx.txt found 9 literals inside 0x9c0, 7 matching and exactly these 2 differing, so this is the complete known divergence set on the TA side. THAT IS NOT SUFFICIENT GROUNDS TO CHANGE THE CONSTANTS, which is why this is a parameter and not an edit: one sighting cannot distinguish 'our constant is wrong' from 'this offset is not a constant'. The control is +0x789, which reads 0x10 in recs2048-gfx.txt and 0x08 in recs4096-gfx.txt -- the identical audit run against the earlier capture reports it as a third mismatch and would have had us pin 0x10, which we now know is wrong. About 4% of the TA record differs between the two captures, and three of the eight differing runs past 0x700 sit within 16 bytes of 0x789, so that whole neighbourhood is per-submit data. Applying the two-observations rule to all nine pinned constants: only the 0x38/0x3a/0x3c tag triple is stable across both captures; 0x789 provably varies; and 0x932, 0x93c, 0x93e, 0x93f, 0x94d have exactly ONE observation each because recs2048's TA block stops at 0x800 and recs4096's descriptor 1 reaches only 0x640. So this flag asks the answerable question -- do these bytes have any effect on hardware -- rather than the unanswerable one. If it proves inert, record that here so nobody re-derives a change from the same capture later. 0: send 0x91/0x01 (default, and what every result to date was measured with), 1: send 0x87/0x03.",
        },
        g17p_render_tvb: u32 {
            default: 2,
            description: "Build the G17P tiler parameter-management graph. 0: diagnostic suppression. 1: reserve aligned TVB storage only. 2 (default): build the macOS-trace/kext-matched graph and bind it through the common TA/3D record header, including blocks, page IDs, block table, control/stats, metrics, HWPB state, counter/discard pages, and Scene allocation.",
        },
        g17p_dep_records: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY. Read the firmware's per-QID dependency records at link 0xfffffc00001261f0 + qid*0x18 (valid byte at +0, completed stamp at +8) for the compute and both render queues, at the render stall. These live in the GFX firmware DATA segment, so unlike the KSM slot bank and the work-channel word -- which firmware RE proves are hardware-owned and unreadable from the host -- they are ordinary DRAM and can be sampled read-only through a page mapping with a cacheline invalidate. This is the record the firmware's dependency rule consults: a dep is satisfied iff the record is valid (byte 0) and dep_stamp <= completed_stamp. It answers the one question the exclusion set leaves open, whether the stuck tiler is dependency-clear: its only dep is the implicit self-dep at stamp 0, and an unwritten record is reported to read as completed = base-1 = 0. If it is dependency-clear and still unpromoted, the KSM front-end refuses to promote even a parent-satisfied slot without new-work. 0: off (default). 1: sample and log.",
        },
        g17p_no_implicit_dep: u32 {
            default: 0,
            description: "DIAGNOSTIC. Omit the kick entry's implicit in-order self-dependency on a queue's FIRST submit. On a first submit the encoder declares {own qid, converted_command_timestamp - 1} = {qid, stamp 0}. Measured at the stall: the stuck tiler's entry dependency word is 0x0000050000000000 = {qid 5, stamp 0}, while the hardware's completed register 0x21028[5] reads 0x862cde4910 -- neither zero nor the firmware's documented never-completed convention of base-1 = 0. Firmware RE says the promotion-side dependency evaluator is in the KSM front-end and NOT in the b1 image, so whether it satisfies stamp 0 against that register cannot be answered statically. This isolates it: with the implicit dependency omitted the tiler's entry declares no parents at all, so if it still refuses to promote, the front-end is not withholding it on a dependency. 0: off (default). 1: omit on first submit only.",
        },
        g17p_render_first_submit: u32 {
            default: 0,
            description: "Deprecated compatibility parameter; ignored. The render now derives outer-slot +0x17 from retained queue lifecycle: 1 on the physical queue's first submit and 0 thereafter, matching every valid native trace.",
        },
        g17p_render_ta_qid_field: u32 {
            default: 1,
            description: "DIAGNOSTIC A/B on TA descriptor +0x8ba, the TA queue's own physical QID. 1 (default): write the real queue id, as native does -- B1 indexes the registered scheduler-resource table with it while admitting the completed TA. 0: write zero, the pre-26675b7 behaviour. The A/B matters because the run that last made the TA EXECUTE and publish real start/end timestamps had this field zero, and setting it correctly moved the failure to 'TA loaded into KSM slot 0 with a pipe and never launched'. That is consistent with the field selecting which QID's scheduler resource the firmware consults at the launch gate: zero consults QID 0's permissive state, five consults QID 5's, which Linux does not yet publish. Use 0 only to reproduce that comparison, never as a fix.",
        },
        g17p_render_slot_template: u32 {
            default: 0,
            description: "EXPERIMENT. Stage the cold-opening queue-context template into the live SKSM ring slot again, and ring the Fragment doorbell before the TA group, as this driver did before 2026-09-02. That combination is the one under which the TA has actually EXECUTED and published real start/end timestamps: the 3D kick declares a barrier on {TA qid, stamp 1}, the fragment doorbell makes the KSM resolve it by reading the TA's ring slot, and it found the template there. Native stages nothing into a live ring slot and sends one doorbell, so this is a divergence -- but if it restores TA execution it isolates the launch trigger to that scan. 0: native shape (default), 1: restore both.",
        },
        g17p_render_seed_ts: u32 {
            default: 0,
            description: "DIAGNOSTIC. Before the render doorbells, write zero into each render queue's completed-timestamp register (SKSM port 0x21028) . A fresh queue's 0x21028 holds a different uninitialised value every boot, while the TA kick entry's implicit parent is {own qid, TS 0}; if the KSM will not make a queue eligible until its completed timestamp reaches that parent, garbage blocks the launch forever, which is exactly the symptom -- slot loaded, pipe armed, nothing started. Native never does this (the firmware seeds the register when it creates the job), so this is only to identify the gate, never a fix. 0: off (default), 1: seed both render queues.",
        },
        g17p_render_kick_qos: u32 {
            default: 0,
            description: "DIAGNOSTIC. Override the two HARDWARE-ONLY QoS bytes in the render kick entry at +0x20[47:32]: class_40 and queue_byte_32. Firmware RE of b1 proves the firmware reads only bits [31:0] of that qword (FUN_a940; FUN_1c850 preserves [63:32]), so these two bytes are consumed by the KSM hardware alone -- a wrong value is invisible to every firmware trace and would produce exactly the observed 'admitted to a slot, never scheduled'. Default 0 keeps the kext-derived values: row 1 of the native 9x3 QoS table, TA=0x0c and 3D=0x18, with queue_byte_32=2 (the HardwareBufferID Linux reserves and publishes in ConfigUpdate +0x56, the qid*8 willSubmit record and here). Those were never validated against a native RENDER entry, because no capture in the corpus contains one; only the CL row 0x08 with HardwareBufferID 1 is proven, by the native compute fixture that dispatches. 1: put the proven compute pair (class_40=8, queue_byte_32=1) on the TILING entry only. 2: on both render entries.",
        },
        g17p_render_tag16: u32 {
            default: 0,
            description: "Publish tag-16, the entry-signal record (selector 0x10), as a fourth ring item on both render queues after their tag-14 AddKicks. The compute path publishes this on first bind -- encode_g17p_compute_optional_event, gated by g17p_first_bind_tag16_required -- and compute is the path that runs; the render path emits only selectors 0x0f (tag 15) and 0x0e (tag 14) and has never published 0x10 at all, which the kext order tag3->15->14->16 puts last, after AddKicks. The record carries {queue_id, old_timestamp} plus 0xfe and reads as a producer-advance notification, so without it the firmware holds a kick entry in the ring with nothing telling it the ring moved -- the published-but-never-started shape the tiler shows, with scan-active stuck at 0, no completion and no fault. Publishing it also raises each queue's published prefix from 3 to 4, so completion requires done >= 4. INFERRED from the compute contract: no capture ever dumped a render tag-16. 0: off (default). MEASURED ON macOS 2026-09-02: a native render writes exactly THREE channel command records per stage -- the command record, tag-15 (selector 0x0f) and tag-14 (selector 0x0e) -- and no selector 0x10 at all, so publishing one diverges from the oracle. The firmware does acknowledge it with KTrace 0x57, and it changed nothing either way. RE-TESTED 2026-09-02 AFTER the priority-2 work-ring fix, so this null is not the pre-fix confound: 0x57 is emitted for BOTH render qids (a=[5,0,0xfe,0] and a=[6,0,0xfe,0]), KTrace 0x42 pulses the same resource-GO strobe a second time, and the tiler still sits in KSM slot 0 at state 0x0100000d phase 6 with TVB 0. Firmware RE identified tag-16 as the only submit-time writer of the KSM resource-GO strobe 0x210d8/0x210e0 that the render path omits; this measurement retires that as the launch gate. 1: on. Applies on BOTH render paths: the SKSM staging path via stage_fragment_kick and the plain path via stage_fragment, with the tiling half covered by build_g17p_late_tiling_bytes in both cases. On the plain path the stamp it names is ordinal+1, the same value tag-14 announces and the firmware echoed back as stamp 1 in its 0x41 receipt.",
        },
        g17p_render_populated_high_root: u32 {
            default: 0,
            description: "DIAGNOSTIC ONLY -- expose the populated driver/kernel TTB1 to the bound render context 1 instead of its distinct empty client-high root. Native AGXUAT allocates separate root0/root1 page tables and mapTaskIdIntoGPTBAT publishes both; the output-positive first-partial trace and m1n1 path prove that the app root1 is empty while the populated firmware high root remains off-table. Therefore 0 is the native topology and the only candidate final design. Setting 1 deliberately overexposes the populated Linux high root to answer one narrow question: did Linux accidentally leave a fragment-consumed pointer in the firmware-high VA domain? If 3D starts, the final fix is to move that pointer into client-low or construct a scoped client high mapping, not to keep this switch enabled. Slot 0, descriptor/tag-15 context IDs, root ownership, and TLBI sequencing are unchanged.",
        },
        g17p_render_sksm: u32 {
            default: 3,
            description: "Install the render QIDs on the KSM bridge and write kick entries, sequenced between tag-15 and tag-14 exactly as compute does. 0: off. 1: legacy compute-shaped command state. 2: bare entry with no RegisterArray bindings. 3: exact captured Metal render shape: zero event mask and kind, TA descriptor+0x60/tag0x47, and 3D descriptor+0xa0/+0x7c0/+0xee0/+0x1600 tagged 0x56/0x10/0x17/0x0a. 4: compatibility alias for 3. Non-zero also maps the render entry rings GPU read-write. MEASURED 2026-09-02, all with a compute first: mode 3 is the only shape that lets the TILER run. Mode 0 publishes no work at all -- the KSM slots stay empty and no completion record of any kind appears. Modes 1 and 2 are strictly WORSE than 3: the tiler itself is then admitted to KSM slot 0 (state 0x0100000d, phase 6) and retired with status 4, tiling timestamps [0x0,0x0], exactly the way the fragment fails under mode 3. So the entry SHAPE decides whether a queue is dispatched classically or admitted to the SKSM scheduler and stuck; do not use 0, 1 or 2.",
        },
        g17p_irq_completion: u32 {
            default: 1,
            description: "Retire a compute submission from the firmware doorbell instead of polling. The submit publishes and returns, the caller waits on a condvar with the runtime mutex released, and the RTKit event worker reads the KSM completion record and wakes it. 0 restores the legacy in-submit poll, which holds the runtime mutex for the whole wait and therefore cannot observe anything the worker would have drained.",
        },
        g17p_ring_normalize: u32 {
            default: 0,
            description: "Fold an item-ring cursor that reads >= the ring entry count back into range instead of refusing it. Default 0 is the conservative behaviour of the 319-deep build: refuse. 1 was intended to survive a consumer reported as exactly `count` at the wrap, but if the firmware can report an out-of-range consumer for any other reason, folding it makes reclaim zero slots the firmware has NOT consumed and a later repeat then overwrites live entries -- which degrades depth sharply and non-deterministically. Only enable it alongside evidence that a cursor really did read `count` at a wrap.",
        },
        g17p_repeat_settle_us: u32 {
            default: 0,
            description: "Sleep this many microseconds after publishing a steady-state submission, before returning to the caller. Exists to test whether submission depth depends on TIMING rather than logic: the 319-deep build emitted several console lines per submission, and on a dockchannel console each of those can cost around a millisecond, so it was padding every submission by several milliseconds. If depth collapses when that padding is removed and returns when this knob replaces it, a race remains after the tag-16 fix and its scale is whatever value restores depth.",
        },
        g17p_ring_limit: u32 {
            default: 0,
            description: "Shrink the compute item ring to this many entries instead of the full 0x500, so the ring WRAP is reached in a few submissions rather than 320. At four records per submission a 32-entry ring wraps on the eighth, and wrap behaviour is size-independent, so this converts a 400-iteration experiment -- which overflows the kernel log and destroys its own evidence -- into a 20-iteration one. Rounded down to a multiple of 4 and floored at 12. 0 uses the full ring.",
        },
        g17p_ring_wrap: u32 {
            default: 0,
            description: "How the outer CL_2 slot reports a queue write index that has just wrapped to 0. 0 (current) sends 0, the plain ring index. 1 sends the ring entry count instead, i.e. reports the head in 1..=count, for a firmware that reads 0 as 'empty' rather than 'wrapped'. This is the exact submission that fails at depth 320: it is the one whose four records fill the last slots and leave the producer at 0.",
        },
        g17p_trace_depth: u32 {
            default: 0,
            description: "Emit the per-submission compute traces (ring cursors, outer channel, retire) only for the first N submissions of a queue. 0 (default) traces every submission, which is what the 319-deep build did -- the default deliberately reproduces it, because console writes cost around a millisecond each on a dockchannel console and therefore pad every submission by several milliseconds, and depth may depend on that padding. Set a small value for deep runs, where thousands of lines otherwise overflow the kernel ring buffer and destroy the evidence at the end of the run; but do not compare a quiet run's depth against a verbose one's without also setting g17p_repeat_settle_us. Timeout samples and publish failures are never suppressed.",
        },
        g17p_completion_stamp: u32 {
            default: 1,
            description: "Gate a compute retire on the firmware's COMPLETION STAMP, not just on the KSM completion record. Firmware RE (b1): the KSM completion record is produced by the KSM hardware block and is consumed by the firmware at the TOP of the completion interrupt; only afterwards does the firmware run its GPU cache-maintenance command (sgx+0x1_8030, polled on 0x1_8038 bit 1) and then store the stamp value from descriptor +0x0F50 through the pointer at descriptor +0x0F40 -- which is the queue support page's dispatch-A word. So the record's arrival says nothing about the dispatch's stores being visible, while the stamp write happens strictly after the flush and does. 1 (default) zeroes that word before publishing and holds the retire until the firmware writes it back, which is what makes completion imply visibility. If the stamp never arrives the retire proceeds anyway with a loud log, so this can only be as bad as 0. 0 restores record-only retire.",
        },
        g17p_completion_stamp_us: u32 {
            default: 500,
            description: "Upper bound, in microseconds, on how long a retire waits for the firmware completion stamp when g17p_completion_stamp=1. This is a bound on a data gate, not a settle: hardware says the stamp is already present at retire for the large majority of submissions, in which case nothing is waited for at all. It is NOT always published, though -- a 300-iteration run left 19 submissions without one -- so the bound is what those cost. 500us is 2.5x the measured 0-200us visibility lag and an order of magnitude cheaper than the 2000us this defaulted to. Exceeding it logs (rate-limited) and proceeds.",
        },
        g17p_completion_trace: u32 {
            default: 1,
            description: "Trace every SUCCESSFUL compute retire: the completion's start/end GPU timestamps, their difference, and the support page's dispatch/status words. Needed because the failure mode is now a submission that retires cleanly while the GPU writes nothing, which a timeout-only diagnostic can never see. A duration that is zero or constant means the firmware retired work the GPU never ran; a plausible varying duration means it ran and the result is invisible to the reader. 2 additionally decodes the whole completion ring on every retire, which is verbose. 0 disables the trace.",
        },
        g17p_repeat_context: u32 {
            default: 1,
            description: "Queue-context handling on a steady-state submission. 1 (default, and what six consecutive passes were achieved with) repoints the item the first bind wrote at this submission's descriptor. 0 leaves it naming submission 1's descriptor. 2 instead writes a COMPLETE item at index = this submission's stamp index, on the reading that the array is a sibling of the SKSM entry array: extent 0x20000 at stride 0x200 is 0x100 items, exactly the geometry's stamp_count and exactly the entry backing's size and stride, and the first bind writes item 1 while consuming stamp index 1. If that reading is right, items 2.. have never been written and every repeat has run against a zero item.",
        },
        g17p_repeat_ring: u32 {
            default: 1,
            description: "Item-ring model for a steady-state compute submission. 1 = append the four records at the live producer, leaving the first bind's records in slots 0..3. 0 = rewind, i.e. zero slots 0..3, put the producer back to 0 and reuse them exactly as the first bind did. The two models are indistinguishable in the macOS captures because every captured process submitted once. Decide between them with the G17P ring[...] consumer value: if it advances after a submission the firmware demonstrably executed, the firmware owns that cursor and append is correct; if it stays 0, rewind is.",
        },
        g17p_repeat_activation: u32 {
            default: 0,
            description: "Also send the priority-2 CL activation (EP 0x21 0x0a) on a steady-state compute submission, not just the direct kick (0x10). Default 0 follows the modelled native lifecycle, where activation is emitted only when the submitted channel identity CHANGES -- so a repeat on the same queue graph sends the kick alone. Set 1 if a repeat publishes but the firmware never consumes it: that is the symptom of the kick alone being insufficient to start a channel scan.",
        },
        g17p_queue_recycle: u32 {
            default: 2,
            description: "How the single hardware compute queue is handed from one DRM client to the next. A DRM client owns the QID4 graph and the slot-1 user-VM binding from its first submit until its queue object is destroyed (queue_destroy or file close). 2 (default) RETAINS the installed QID4 graph and hands over only the user-VM binding: the outgoing client's binding and timestamp page are released, the incoming client binds its own VM into slot 1 (and the ctx2/ctx3 compute aliases), and the submission takes the steady-state chronology -- tag 3 -> 15 (install flag CLEAR) -> 14 -> 16 -> CL_2 appended at the live item-ring producer -- exactly as a same-client repeat does. This is the only mode that never re-runs the first-bind chronology against a QID the firmware already has installed. 1 recycles the queue graph instead: the previous owner's binding and timestamp page are released, QID4 is taken off the SKSM bridge, its storage is RETIRED (kept allocated, never reused) and a fresh PreparedMemory graph is built, whose first submission re-runs the first-bind chronology with the tag-15 queue-install flag SET on an already-installed QID -- which is what makes the second DRM client publish cleanly and never complete. 0 forces a full recover_persistent_runtime, i.e. both firmware CPUs stop and the session is rebuilt; note that this does NOT clear the firmware/KSMFE per-QID install either, because unregister_compute_sksm_queue's disable transaction is a suppressed no-op whenever g17p_sksm_mmio=0. Mode 2 falls back to mode 1 when the retained graph is unusable (crashed session, an abandoned or in-flight submission, or a queue that was never installed), and mode 1 falls back to mode 0 on error.",
        },
        g17p_zero_duration: u32 {
            default: 1,
            description: "CONFIRMED ON RENDER 2026-08-31: with g17p_render_service_events=1 and g17p_recovery_allow_active_scheduler=1 both render queues RETIRE -- the KSM completion ring names G17P_COLD_OPENING_TA_DESCRIPTOR_BASE (0xfffffc20c0018000) and G17P_COLD_OPENING_3D_DESCRIPTOR_BASE (0xfffffc20c00b0000) in separate records with end timestamps 12 ticks apart -- while BOTH carry start=0x0. That is this parameter's retired-without-executing signature, now reproduced on render: retired=true (both queue prefixes advanced), observed=false (RenderExecutionState::observed requires every timestamp non-zero), so the render wait times out CORRECTLY and there is no ordinal or selector bug behind it. The GPU did not execute; the recovery retired the kicks and the advancing end timestamps are retirement bookkeeping. Second independent confirmation that the absent tiler parameter buffer is the cause: a tiler with nowhere to bin geometry never starts, so no start timestamp is ever stamped. Note also that these records landed on completion ordinal 0 although the render SKSM queues are configured with RENDER_COMPLETION_SELECTOR = 2 (g17_manager.rs), pointing at the CLASSIC item-ring path having retired them rather than the SKSM half. ORIGINAL: what to do when a compute submission retires with EQUAL start and end GPU timestamps, i.e. the firmware retired work the GPU never executed. Measured on J700: a healthy add3 retires with duration 0xda-0xdd ticks; every submission after a fault-blamed firmware recovery retires with duration 0 and leaves its destination buffer untouched, while the queue/firmware alignment either side of that recovery is byte-identical. 0: log and mark only, and still report the submission as successful -- the previous behaviour, and a silent wrong answer. 1 (default): log it and fail the submission with ETIMEDOUT rather than reporting success, but do NOT rebuild the firmware session. 2: additionally force a session rebuild at the next client handoff. Mode 2 is NOT the default because a rebuild attempted from a gated session has been measured to FAIL after ~1.02s -- one apple_rtkit_wait_for_completion timeout inside apple_rtkit_boot, waiting for an IOP HELLO/EPMAP the restarted ASC never sends -- and a failed rebuild is destructive: it has already dropped both ASC CPUs and both RTKit owners and never reaches resume_submissions, so the device is dead for the rest of the boot. An immediate honest error beats a one-second stall that kills the device.",
        },
        g17p_gate_quiesce_ms: u32 {
            default: 0,
            description: "After a zero-duration retire has shown the core power gate is closed, publish nothing for this many milliseconds before the next submission. FW RE of b1: pause bit 3 has exactly ONE clear site in the whole image -- 0x25dd8, inside the scheduler thread idle descent, reached only from 0x25c00 and the cbnz->0x25dc0 routes -- and the generic pause helper 0xa1c4(set,bit) is never called with bit 8 anywhere, so no host message can clear it directly. The descent guards at 0x25940 are queue state the driver does drive, though: it descends only when the queue producer equals the consumer and two other words are clear, and submitting continuously is what pins the thread to the work path at 0x25e2c -- which is why no 0x46 record is emitted at all after a fault. Going quiet lets the thread reach a genuine idle whose EXIT clears bit 3, leaving mask 0x2, from which the IdlePowerOff(0) lever is the clear that reaches zero and therefore actually calls the core power sequencer 0x20a70. Use with g17p_power_wire=2. 0: off. Try 200-1000.",
        },
        g17p_recovery_resync: u32 {
            default: 1,
            description: "Bitmask. What to resynchronise on the RETAINED compute queue after the driver services a firmware recovery, instead of assuming the queue and the firmware still agree. A recovery is raised by a GMMU fault, and after one every later kick on the retained graph is accepted, retired, and never executed -- while a boot with no fault runs any number of sequential clients. bit0 (1, default): mark the queue's predecessor UNKNOWN, i.e. clear producer.previous_valid, so the next kick encodes its implicit self-dependency as the no-predecessor form (converted_command_timestamp - 1 = 0, dependency on QID stamp 0) that a proven first bind sends, rather than depending on a stamp the firmware's event machine may no longer acknowledge across a recovery. That single dependency qword is the only difference between a post-recovery repeat and a working first bind. bit1 (2): additionally align producer.current_timestamp/base_timestamp FORWARD to the firmware's per-QID last-submitted HW timestamp (primary main-config +0x274, bundle view 4, QID 4 at table offset 0x40) when that entry reads valid; never backwards, and a no-op while the entry reads invalid, which is what it read the last time it was sampled. bit2 (4): additionally clear the cached channel identity so the next submission re-sends the priority-2 CL activation (EP 0x21 0x0a) as well as the direct kick -- it only affects the notification predicate, never the tag-15 queue-install flag. 0 disables all of it and restores the previous behaviour, where the graph marches on across a recovery. Bisect ladder: 0, 1, 3, 5, 7.",
        },
        g17p_abandon_recovery: u32 {
            default: 1,
            description: "Recover from a client whose compute submission was published and never seen to retire, instead of poisoning the rest of the boot. 1 (default): the outgoing client's binding is QUARANTINED -- its UAT slot is handed back immediately while its page tables and mapped GEM objects stay allocated, so nothing the firmware might still name is freed and no CPU stop is needed -- the retained QID4 graph has the abandoned kick written off its outstanding count, its armed completion stamp disarmed and its stale KSM record cleared, and the next client rebinds onto it exactly as it would after a clean handoff. A same-owner retry after an abandoned kick performs the same write-off without quarantining, since that client still owns its own memory. It also lets a crashed session be rebuilt (at most four times per module load) when a NEW client queries the device, so a dead session no longer means ENODEV for every later process until a reboot. 0 restores the previous behaviour: an abandoned submission forces the next handoff down the full rebuild path, which re-asserts the tag-15 queue-install flag on an already-installed QID and therefore breaks every client after the one that failed.",
        },
        g17p_sksm_mmio: u32 {
            default: 0,
            // permissions: 0,
            description: "G17P SKSM port MMIO (0: suppressed, 1: enabled). Host writes to                           sgx+0x4090/0x4098 take an asynchronous SError on J700; the whole                           sgx 0x0..0x2ffff range is unreachable from the AP. Default off so                           the driver stays alive; set 1 only to re-test the port.",
        },
    },
}
