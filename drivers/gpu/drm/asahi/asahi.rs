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
mod g17_config;
mod g17_compute;
mod g17_drm;
mod m5_drm;
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
mod g17_render;
mod g17_resources;
mod g17_rtkit;
mod g17_submission;
mod g17_trace_capture;
mod g17_uapi;
mod g18_compute;
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
        // cycle once already.
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
            description: "Dump render descriptor bytes",
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
