#!/usr/bin/env python3
"""Source boundary regressions; pure archive bytes are tested by Rust itself."""
from pathlib import Path
import unittest

SOURCE = Path(__file__).resolve().parents[1]


def function(text, name):
    start = text.index("fn " + name + "(")
    body = text.index("{", start)
    depth = 1
    end = body + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[body:end]


class CaptureLifecycle(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.live = (SOURCE / "g17_live_boot.rs").read_text()
        cls.manager = (SOURCE / "g17_manager.rs").read_text()
        cls.drm = (SOURCE / "g17_drm.rs").read_text()

    def test_prepared_scan_precedes_flist_wake(self):
        body = function(self.live, "submit_translated_render")
        begin = body.index(".begin_translated_render(")
        scan = body.index(".capture_prepared_render_trace(")
        flist = body.index("self.publish_render_usc_freelist_request()")
        self.assertLess(begin, scan)
        self.assertLess(scan, flist)
        self.assertEqual(body.count(".capture_prepared_render_trace("), 1)
        self.assertIn("native_doorbell_mode == 0 && !split_launch", body[scan:flist])

    def test_halt_scan_after_validation_before_ack(self):
        body = function(self.live, "complete_primary_real_recovery")
        scan = body.index("finish_g17p_render_trace(")
        self.assertLess(body.index("manager.validate_empty_primary_firmware_recovery(generation)?"), scan)
        self.assertLess(body.index("if cause.host_recovery != 0"), scan)
        self.assertLess(scan, body.index("manager.acknowledge_primary_firmware_recovery(1, 2)?"))

    def test_both_completion_paths_capture_before_flist_release(self):
        body = function(self.live, "wait_render_completion_servicing_events")
        self.assertEqual(body.count("finish_g17p_render_trace("), 2)
        chunks = body.split("finish_g17p_render_trace(")[1:]
        for chunk in chunks:
            self.assertIn("Phase::AfterPairRetirement", chunk)
            self.assertLess(chunk.index("Phase::AfterPairRetirement"),
                            chunk.index(".retire_render_usc_freelist_at_completion()?"))

    def test_terminal_export_takes_owner_and_seals_before_return(self):
        body = function(self.manager, "finish_render_trace")
        self.assertIn("trace::Phase::AfterPairRetirement | trace::Phase::RecoveryBeforeAck", body)
        self.assertIn("capture.prepared", body)
        self.assertLess(body.index("self.capture_render_trace_observation"), body.index("self.trace_capture.take()"))
        self.assertLess(body.index("archive.seal()"), body.index("Ok(Some(capture))"))
        self.assertNotIn("GFP_", body)

    def test_no_new_mmio_or_physical_alias_in_capture_methods(self):
        for name in ("capture_prepared_render_trace", "capture_render_trace_observation", "finish_render_trace"):
            body = function(self.manager, name)
            for forbidden in ("Mmio", "ioremap", "LiveFirmware", "map_at(", "pr_info!", "dev_info!"):
                self.assertNotIn(forbidden, body)

    def test_render_capture_uses_execution_context_and_retained_aliases(self):
        source = (SOURCE / "g17_resources.rs").read_text()
        source = source[source.index("impl G17PUserRenderStorage {"):]
        body = function(source, "capture_owned_trace")
        self.assertIn("let context = u32::from(self.descriptor_context_id)", body)
        self.assertNotIn("self._command_context.context_id()", body)
        for owner in ("deflake", "rt_memory", "tpc", "aux_fb"):
            self.assertIn("&self.render_user_aliases." + owner, body)
        self.assertIn("self.fragment_status_low.iova()", body)
        self.assertIn("G17P_RENDER_STATE_TA_STATUS", body)
        self.assertIn("self.render_state.capture_trace_range(", body)
        self.assertIn("map(|pm| &mut pm.backing), 0)", body)

    def test_render_submission_uses_the_logical_queue_execution_context(self):
        body = function(self.drm, "submit")
        call = body[body.index(".submit_translated_render("):]
        self.assertRegex(call[:800], r"\.context\s*\.clone\(\)")
        self.assertNotIn("RENDER_APP_CONTEXT", call[:800])

        source = (SOURCE / "g17_resources.rs").read_text()
        owner = source[source.index("impl G17ResourceOwner {"):]
        constructor = function(owner, "new_user_render_storage")
        self.assertIn("context.matches(vm)", constructor)
        self.assertIn("context.context_id()", constructor)
        restage = function(source, "restage_command")
        self.assertEqual(
            restage.count("context_id: u32::from(descriptor_context_id)"), 2
        )
        self.assertIn("self.descriptor_context_id = descriptor_context_id", restage)

    def test_boot_capture_is_before_initdata_not_before_rtkit_boot(self):
        start = self.live.index("let mut manager = g17_manager::G17PManagerConstruction::new_with_uat(")
        self.assertLess(start, self.live.index("let handoff = manager.handoff();", start))
        self.assertLess(self.live.index("let handoff = manager.handoff();", start),
                        self.live.index(".send_message(g17_rtkit::ENDPOINT_GFX_MESSAGES, gfx_init)", start))
        self.assertIn("trace::Phase::BootConstructed", function(self.manager[self.manager.index("impl G17PTraceDump"):], "new"))

    def test_pm_low_aliases_are_explicit_context_zero_owners(self):
        source = (SOURCE / "g17_resources.rs").read_text()
        body = function(source[source.index("impl G17ResourceOwner {"):], "capture_owned_trace")
        for owner in ("pb_descriptor_table_low", "uma_page_pool_descriptor_table_low", "partial_primary_index_low",
                      "completion_ordinal_0_low", "completion_ordinal_2_low"):
            self.assertIn("&self.resources." + owner, body)
        self.assertIn("metrics.context = 0", body)
        self.assertIn("metrics.dva = self.resources.parameter_metrics_low_va", body)


if __name__ == "__main__":
    unittest.main()
