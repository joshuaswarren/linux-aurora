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
mod g17_asc_bringup;
mod g17_boot;
mod g17_completion;
mod g17_config;
mod g17_compute;
mod g17_drm;
mod g17_firmware;
mod g17_initdata;
mod g17_job;
mod g17_status;
mod g17_queue_limits;
mod g17_channel;
mod g17_lifecycle;
mod g17_live_boot;
mod g17_manager;
mod g17_perf;
mod g17_power_preflight;
mod g17_rtkit;
mod g17_submission;
mod g17_trace_capture;
mod g17_uapi;
mod g18_compute;
mod g18_compute_inputs;
mod g18_compute_metadata;
mod g18_compute_plan;
mod g18_compute_memory;
mod g18_compute_state;
mod g18_compute_payload;
mod g18_compute_triangle;
mod g18_graphics_payload;
mod g18_address_map;
mod g18_render;
mod g18_application;
mod g18_drm;
mod g18_application_runtime;
mod g18_render_command;
mod g18_render_assembly;
mod g18_render_stage;
mod g18_render_passthrough;
mod g18_render_policy;
mod g18_render_pixels;
mod g18_parameter_buffer;
use g17_resources::g18_graphics_queues;
mod g18_scheduler;
mod g18_live_boot;
mod g18_uma;
mod gem;
mod gpu;
mod hw;
mod identity;
mod initdata;
mod mem;
mod m5_channel;
mod m5_compute;
mod m5_compute_program;
mod m5_graphics_bundle;
mod m5_render;
mod m5_completion;
mod m5_sksm;
mod m5_vm;
mod m5_render_target;
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
        m5_fw_trace: u32 {
            default: 0,
            description: "M5 primary trace classes: power bit 0, submission bit 1, recovery classifier bit 9",
        },
        m5_initdata_test: u32 {
            default: 0,
            description: "M5 fresh RAM boot: 1=initdata ACKs, 2=openings, 3=queue, 4=scheduler, 5=context, 6=one compute, 7=setup without kick, 8=request stop after admission, 9=live status, 10=live firmware monitors, 11=compute triangle, 12=TA/fragment triangle; 1..12 request stop; 13=retained session, 14=experimental DRM first-job adapter",
        },
        g18_initdata_test: u32 {
            default: 0,
            description: "T8152/J873 G18G bring-up stage. 0 refuses before MMIO/DMA. 1=read-only powered-resource probe. 2=RTKit. 3=initdata ACKs. 4=openings, idle policy, 60 s observation. 5=empty queue. 6=queue plus scheduler binding. 7=owned empty UMA pool binding. 8=compute diagnostic. 9=H1 render replay. 10=firmware plus application DRM render node. Stages 5/6/7 then observe without GPU jobs. Firmware stages require prior power, loader receipt and apple_mailbox.ascwrap_v8_cpu_control=1; all owners remain retained until reboot. 90=SGX power-on; 92=GFX1 power-on; 91=GFX power-on; 93=idle requests. Power stages are never implied. Mutating stages require user-authorized bring-up. Other values refused",
        },
        g18_compute_variant: u32 {
            default: 0,
            description: "Stage 8 compute diagnostic. 0=captured compute-add. 1=empty stream: captured barrier and terminate replace the launch; all output must remain poisoned. 2=64x64 compute triangle using a separately verified private shader patch; no TA/3D or Mesa. Variants 1 and 2 require g18_initdata_test=8. Other values are refused",
        },
        g18_compute_owner_test: u32 {
            default: 0,
            description: "J873 allocation-only E2E before firmware publication: 1 verifies shared-compute backing, guarded directory growth, allocator rollback and process aliases. Never submits GPU work. Other nonzero values refused",
        },
        g18_render_variant: u32 {
            default: 0,
            description: "Stage 9 H1 diagnostic: 0=colour, 2=terminated VDM plus background clear. Other variants refused before MMIO. One asynchronous worker pass, captured/M5 H1 policy, all owners retained; see G18-RENDER-RUNTIME.md",
        },
        g18_render_profile: u32 {
            default: 1,
            description: "Stage 9 G18GFX01 v2 profile: 1=colour-a, 2=colour-b, 4=fragment-witness (colour only). Gears profile3 is offline only and refused by H1. Stage0 remains the default",
        },
        g18_admit_compute: u32 {
            default: 0,
            description: "G18 DRM adapter: 1 admits single-dispatch compute commands (K1). 0 refuses them",
        },
        g18_admit_msaa4: u32 {
            default: 0,
            description: "G18 DRM adapter (g18_initdata_test=10 only): 1 admits exactly the native 4x MSAA render (4 samples, 32x16 utiles, sample positions 0xeaa26e26, 8-byte samples, one layer, no depth/stencil) with its native 4x command fields; Mesa needs AGX_G18_MSAA4=1 to submit it. 0 refuses every multisampled render (default)",
        },
        g18_usc_statistics: u32 {
            default: 0,
            description: "Stage 9 colour fixture only: 1 enables owned TA/3D SMAPP statistics pages. Raw memory statistics, not shader invocation counts. Default 0",
        },
        g18_sma_minimum: u32 {
            default: 0,
            description: "Stage9/profile4 or stage10/profile1, variant0: 1 probes fixed native minimum TA/fragment resource words with unchanged owned backing. Default 0",
        },
        g18_graphics_stall_trace: u32 {
            default: 0,
            description: "Stage9/profile4 or stage10/profile1, variant0: 1 enables firmware category9 diagnostics and the native TA mode2 progress sampler. No timeout changes. Default 0",
        },
        g18_vdm_high_alias: u32 {
            default: 0,
            description: "Stage9/profile4/variant0 with minimum SMA and stall tracing: 1 fetches unchanged native VDM bytes through the Gallium-address alias. Diagnostic only. Default 0",
        },
        g18_ppp_high_alias: u32 {
            default: 0,
            description: "With the stage9 high-VDM control: 1 aliases unchanged native PPP pages into the Gallium range and relocates only their VDM pointers. Diagnostic only. Default 0",
        },
        g18_sma_ideal: u32 {
            default: 0,
            description: "Stage9 variant0 profile4 only: 1 probes native ideal graphics resource policy with 40MiB backing and precommitted directory growth. Default 0",
        },
        g18_native_barrier: u32 {
            default: 0,
            description: "Stage9 variant0 profile4 with ideal SMA only: 1 probes the native same-group TA-to-3D barrier kind3. Default 0",
        },
        m5_graphics_frames: u32 {
            default: 1,
            description: "Number of sequential mode12 renders on a fresh RAM boot (1..64); reset pixels between completed frames",
        },
        m5_graphics_context: u32 {
            default: 3,
            description: "Application root slot for the mode12 regression (3..63); ordinary application queues allocate their own slots",
        },
        debug_flags: u64 {
            default: 0,
            description: "Debug flags",
        },
        fault_control: u32 {
            default: 0xb,
            description: "Fault control (0x0: hard faults, 0xb: macOS default)",
        },
        initial_tvb_size: usize {
            default: 0x8,
            description: "Initial TVB size in blocks",
        },
        robust_isolation: u32 {
            default: 0,
            description: "Fully isolate GPU contexts (limits performance)",
        },
        g17p_simplefb_iova: u64 {
            default: 0,
            description: "Diagnostic simplefb GPU address override; 0 disables it",
        },
        g17p_test_stage: u32 {
            default: 2,
            description: "G17P bring-up stage (0: control, 1: QID4, 2: full driver path)",
        },
        g17p_max_recoveries: u32 {
            default: 0,
            description: "Maximum firmware recoveries per session; 0 is unlimited",
        },
        g17p_fw_log: u32 {
            default: 0,
            description: "Enable the GPU firmware log ring",
        },
        g17p_fw_trace: u32 {
            default: 0,
            description: "Firmware trace class mask; 0 disables capture",
        },
        g17p_binary_capture: u32 {
            default: 0,
            description: "Export first-render binary capture through devcoredump; allocates 64 MiB; bit 1 also reads the boot firmware code window",
        },
        g17p_fw_ktrace_verbose: u32 {
            default: 0,
            description: "Print all captured KTrace records instead of the bounded selection",
        },
        g17p_fw_ktrace_budget: u32 {
            default: 32,
            description: "Maximum ordinary KTrace records printed per drain",
        },
        // NOTE: every `permissions:` line in this block is commented out, so
        // the macro gives each parameter permissions 0. That means NONE of
        // these appear under /sys/module/asahi/parameters -- they can only be
        // set on the insmod command line. Looking for one in sysfs and not
        // finding it does not mean the build lacks it; check
        // `strings asahi.ko | grep parmtype=` instead. This cost a hardware
        // cycle once already. The exceptions are registered outside this
        // block: the 0644 g18_app_* application-compute knobs
        // (g18_application_compute_resources.rs) ARE in sysfs and are read
        // at every compute submission.
        g17p_vm_probe_va: u64 {
            default: 0,
            description: "Diagnostic GPU virtual address to inspect; 0 disables the probe",
        },
        g17p_vdm_dump: u32 {
            default: 0,
            description: "Dump submitted VDM command bytes",
        },
        g17p_fault_report: u32 {
            default: 0,
            description: "Fault diagnostics: bit 0 includes submission dumps; bit 1 enables recovery-only register snapshots; bit 2 captures fault-only B1 scheduler/FList DRAM",
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
            description: "Render state dump size in KiB; 0 disables it",
        },
        g17p_dump_hwdata: u32 {
            default: 0,
            description: "Dump firmware hardware-data objects",
        },
        g17p_perf_window: u32 {
            default: 0,
            description: "Submissions per throughput report; 0 disables accounting",
        },
        g17p_render_transition_probe: u32 {
            default: 0,
            description: "Diagnostic render mutation reason logging; 0=off, 1=observe (no drain unless a single class is selected)",
        },
        g17p_render_transition_class: u32 {
            default: 0,
            description: "Diagnostic single class: 0=observe, 1=registration, 2=context handoff, 4=new PB graph, 8=FList init, 16=TVB growth, 32=desync; compound or unknown reasons never drain",
        },
        g17p_compute_burst_probe: u32 {
            default: 0,
            description: "Diagnostic: 0=off, 1=log steady compute publication timing, 2=rendezvous 3 steady queues before admission (20ms abort, at most 64 cohorts); no execution cap",
        },
        g17p_mixed_resource_probe: u32 {
            default: 0,
            description: "Read-only mixed-resource census: 0=off, 1=compute fault only, 2=also compute ordinals 0/1 and every 16; both render owners, FList/UMA and HBIDs; no execution gating",
        },
        g17p_split_execution_id_probe: u32 {
            default: 0,
            description: "Diagnostic only: separate compute context ID for the same VM/root, retaining logical scheduler/QoS/PID; costs one extra context slot per compute owner; no execution gating",
        },
        g17p_render_poll_us: u32 {
            default: 0,
            description: "Completion poll interval in microseconds; 0 selects adaptive backoff",
        },
        g17p_render_wait_ms: u32 {
            default: 2000,
            description: "Render completion timeout in milliseconds",
        },
        g17p_verbose: u32 {
            default: 0,
            description: "Per-submission diagnostic dumps. The render path emits several dozen lines for every submission -- the QoS regions, both status pages, the PM/HWPB and PBDesc snapshots, the tag-15 and SKSM records and the context roots. That is the bring-up instrumentation this driver was written with, and at desktop frame rates it costs real time and buries every other message in the ring buffer within a few seconds. 0: only lifecycle and failures (default), 1: the full per-submission dumps.",
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
        g17p_render_recovery_retire: u32 {
            default: 0,
            description: "PROBE, and SELF-GATING -- it cannot act unless the evidence that justifies it is present. `retire_abandoned_render_if_firmware_retired` reaches its `true` through `!completion.retired || !state.job_list_empty || execution.observed()`, and `observed()` CONTAINS `job_list_empty`, so the guard demands job_list_empty true and observed() false at once -- satisfiable only when a timestamp is zero. It is a nop-retire classifier and its own `g17p_render_nop_retire` documentation says so: it answers `true` only for a group the firmware retired WITHOUT executing. The recovery-blamed settle asks it a question it is structurally incapable of answering: a stamp-blamed recovery serviced with a live render scheduler deliberately SKIPS the generation-0 timestamp wipe so the render's progress markers survive, so the timestamps are non-zero by design, `observed()` is true, and `firmware-retired=false` is arithmetic rather than evidence. The caller reads that as `still owned by firmware`, poisons the client's VM, raises `session_recovery_pending` and arms a rebuild that is itself known to fail -- and the quarantine protects nothing, because releasing the blamed group is exactly what a recovery IS. 0 (default): unchanged. 1: on the RECOVERY-BLAMED settle path only, accept a group that executed -- drop the `observed()` term and keep BOTH real proofs that the firmware is finished, `completion.retired` (both queue cursors at the published target) and `state.job_list_empty` (the job unlinked from the firmware completion list). Those two are what make the host bookkeeping safe; `observed()` only ever described whether the GPU did any work, which does not bear on ownership. Because the `job_list_empty` term is retained, this parameter is INERT on exactly the outcome that would refute the diagnosis: if the wedge turns out to show `job-list-empty false`, the firmware really does still hold the group and this changes nothing. Read the `settle-reason recovery-blamed` line first; promote to default only once that line has shown job-list-empty true with four non-zero timestamps on a real wedge.",
        },
        g17p_compute_config_context: u32 {
            default: 1,
            description: "Select the hardware context declared by the compute tag-15 ConfigUpdate at record +0x32. 1 (default) declares the submitting queue\'s independently leased application context, matching the context word and TTBAT roots used by the same kick. 0 retains the historical fixed context 2 for diagnosis only. Contexts 2/3 are global aliases whose roots are repointed between clients; declaring 2 from every queue lets a later client detach an earlier queue from its VM. The production path therefore uses the queue context and relies on range-scoped VM_BIND invalidation to make mappings added after context allocation visible to that ASID.",
        },
        g17p_quarantine_scope: u32 {
            default: 1,
            description: "PROBE. How far a single quarantined compute queue is allowed to reach. One lost submission on one QID currently kills the whole device twice over: `quarantine_async_compute` raises `session_recovery_pending`, which makes `is_crashed()` true so every new client gets ENODEV and every existing one EIO and the (known-broken) session rebuild is armed; and `check_async_compute_submission` refuses EVERY client's compute submit while `async_compute_queues.iter().any(|q| q.quarantined)`. Neither is needed to keep firmware-referenced memory safe: a quarantined queue is permanently retired -- `release_async_compute_queue` will not strip it, `reusable_idle_compute_graph` and `rebindable_released_compute_graph` both exclude it, and `prepare_async_compute_queue` skips it -- so nothing it owns is ever freed or reused, with or without a session rebuild. The cost of scoping is one leaked QID per quarantine, bounded by MAX_QUEUES (58); the cost of not scoping is a desktop that dies for two minutes of idle. The render side has the same shape and the same escalation: a render submission the driver cannot prove the firmware finished with raises `session_recovery_pending` through `render-submission-lost`, which is the route that killed the desktop on the 236s soak. There the device-wide flag buys something real -- there are only two render slots, and the processor-stop boundary it reaches is what rehabilitates a quarantined one, so scoping trades a permanently halved (or, twice, exhausted) render capacity for a session that stays up. That trade is only worth taking because the rebuild the flag arms is itself known broken: it ends in `apple_rtkit_boot` ETIME and ENODEV for the rest of the boot, so today the escalation costs the device immediately and returns nothing. BITMASK. Bit 0 is now the DEFAULT, on this evidence: across repeated boots on G17P 2026-09-18 a single fault produced 269 `stage=acquire-lease errno=-5` failures and a dead GPU with the bit clear, and 0 with it set, consistently. Its documented cost -- that bit 0 also skips arming the session rebuild -- is covered by `g17p_clear_poison_on_recovery`, which releases the latch when a firmware recovery acknowledges its final clear, so recovery still happens and now actually rehabilitates. Set 0 for the device-wide behaviour, byte-identical to the previous driver. Bit 0 (1): scope the COMPUTE quarantine -- the submission gate refuses only the owner whose own queue is quarantined, the quarantine does not raise `session_recovery_pending`, and the `compute_faulted` latch stops reaching the RENDER lease path and `submission_error`. That third arm was missing until it was measured on G17P 2026-09-18: with bit 0 set, a scoped quarantine on qid=8 still failed an unrelated client at `stage=acquire-lease errno=-5` 27s later and still lost a freshly launched render-only client to DEVICE_LOST, because `G17PComputeLeaseGuard::quarantine()` sets `compute_faulted` unconditionally and `acquire_lease` reads it for every render submission on the device. `acquire_compute` still reads it unscoped, so a client parked in `lease_free.wait_interruptible` is still woken and failed rather than left asleep on a compute install that will never complete. Bit 1 (2): scope the RENDER loss -- the losing client still has its VM status poisoned and its slot quarantined, which is correct and stays, but the session is not marked crashed for everyone else. 3: both. UNTESTED ON HARDWARE at the time of writing: run it against the bare-desktop reproducer and compare how long the session survives, and read the `G17PMARK quarantine-scoped` lines to confirm the path was taken. Do not promote any value to default on a single green run.",
        },
        g17p_quarantine_selftest: u32 {
            default: 0,
            description: "DIAGNOSTIC, default off. Reproduction oracle for the compute-quarantine blast radius, which is otherwise only reachable by waiting for a real firmware timeout. Set to N > 0 and the Nth SUCCESSFUL async-compute completion of the boot additionally runs the device-wide arm of the quarantine -- `G17PComputeLeaseGuard::quarantine()` plus the per-queue retirement -- at the real call site, and then completes its own submission normally. Completing the trigger is deliberate: the client that fired it is not the one under test, so any later failure is unambiguously the poison reaching somebody else. Look for `G17PMARK quarantine-selftest`. With `g17p_quarantine_scope` clear this must kill every later client, including a brand-new render-only one, at `stage=acquire-lease errno=-5`; with bit 0 set those clients must keep running. It fires once per boot and costs one leaked QID. Never set this on a machine doing real work.",
        },
        g17p_clear_poison_on_recovery: u32 {
            default: 1,
            description: "Clear the device-wide compute admission poison (`compute_faulted`) when a primary firmware recovery reports its final clear with state=0. Before this, the poison was cleared ONLY by `rehabilitate_after_processor_stop`, i.e. by a full session rebuild -- and that rebuild is known to end in `apple_rtkit_boot` ETIME and ENODEV for the rest of the boot. So a single quarantined compute queue left the latch set forever, and every later client -- including brand-new ones that never submitted anything -- took EIO from `acquire_lease`, `submission_error` or a `get_vm_status` query that recorded the device error into its own clean VM. Measured on G17P 2026-09-18: 69 distinct VMs poisoned in one boot, and `G17PMARK vm-status poisoned by QUERY: vm 1 was clean` followed 0.43s later by 100 `stage=acquire-lease` failures. A recovery that acknowledges its final clear at state=0 is a real rehabilitation boundary: the firmware is alive and reset, which is exactly why every submission straddling it retires with zero duration. Default 1 because the previous behaviour is a permanently dead GPU; set 0 to restore it.",
        },
        g17p_recovery_snapshot: u32 {
            default: 0,
            description: "Capture halted firmware recovery state before acknowledgement",
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
            default: 8,
            description: "Emit the per-submission compute traces (ring cursors, outer channel, retire) only for the first N submissions of a queue. Default 8: trace the opening submissions of each queue, then go quiet. 0 traces EVERY submission, which is what the 319-deep bring-up build did and what this defaulted to until now -- console writes cost around a millisecond each on a dockchannel console and so pad every submission by several milliseconds. Measured on G17P 2026-09-18 under ordinary CTS load, that default produced 2.6 MILLION kernel log lines per minute (43k lines/second), which is a per-submission cost in the shipping path and not a diagnostic anyone was reading. Bring-up depth results may depend on that padding, so set 0 to reproduce them, and do not compare a quiet run's depth against a verbose one's without also setting g17p_repeat_settle_us. Set a small value for deep runs, where thousands of lines otherwise overflow the kernel ring buffer and destroy the evidence at the end of the run; but do not compare a quiet run's depth against a verbose one's without also setting g17p_repeat_settle_us. Timeout samples and publish failures are never suppressed.",
        },
        g17p_compute_scs_pages: u32 {
            default: 0,
            description: "DIAGNOSTIC: compute SCS scratch request in 4K pages, 1..65535. Zero disables the request and ignores all other SCS knobs, preserving the historical register program. Nonzero requests are experimental and do not advertise ray-tracing support. Serialize changes with GPU tests; set pages last when enabling a request.",
        },
        g17p_compute_scs_mib: u32 {
            default: 0,
            description: "DIAGNOSTIC: SCS MiB field in register 0x101d1, 0..65535. Zero derives ceil(pages / 256), matching AGXMetalG17P. A nonzero value overrides the derivation for controlled experiments. Ignored while SCS pages is zero.",
        },
        g17p_compute_scs_extras: u32 {
            default: 0,
            description: "DIAGNOSTIC: SCS extras word in register 0xd479. Ignored while SCS pages is zero.",
        },
        g17p_compute_scs_window: u32 {
            default: 0,
            description: "DIAGNOSTIC: SCS window field, 0..255, placed below the enable bit in register 0x107a1. Ignored while SCS pages is zero.",
        },
        g17p_compute_usage_slot: u32 {
            default: 0,
            description: "Enable per-kick compute scratch-usage accounting",
        },
        g17p_completion_stamp_us: u32 {
            default: 500,
            description: "Maximum completion-stamp visibility wait in microseconds",
        },
        g17p_completion_trace: u32 {
            default: 0,
            description: "Completion logging level: 0 off, 1 summary, 2 detailed",
        },
    },
}
