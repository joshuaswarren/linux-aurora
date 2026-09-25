#!/usr/bin/env python3
"""Compile the real g17_initdata.rs and run the G18G (J873, 26A428) layout tests.

The tests live in g18_initdata_test.rs, which g17_initdata.rs includes as
`g18_tests` under #[cfg(test)]. They prove two things offline:

  * regression: the layout functions over the 25G76 allocation-size table
    reproduce every literal M5 constant, and the J700/J813 encoders stay
    bit-identical (golden FNV-1a hashes of every encoder output);
  * G18G: every hw_data/region-C offset the 26A428 b000/b010 copiers read lands
    on a field the encoder writes, and no unrelocated J813 value is left behind.

The whole module (the pre-existing g17_initdata tests included) is then run so
a G18G change can never silently break the M5 path.
"""
from pathlib import Path
from g17_config_test_support import with_g17_config
import subprocess
import tempfile

root = Path(__file__).resolve().parents[1]
source = r'''
#![allow(dead_code, non_upper_case_globals)]
mod module_parameters {
    pub struct Param(u32);
    impl Param { pub fn value(&self) -> &u32 { &self.0 } }
    pub static g17p_fw_log: Param = Param(1);
    pub static g17p_progress_watchdog: Param = Param(1);
    pub static g17p_idle_selector: Param = Param(0);
}
'''

G18_TESTS = (
    "m5_shared_layout_reproduces_the_literal_25g76_constants",
    "m5_private_layout_reproduces_the_literal_25g76_constants",
    "m5_register_window_vas_follow_the_allocation_rule",
    "j700_and_j813_encoder_outputs_are_bit_identical",
    "g18g_layouts_match_the_26a428_size_table",
    "g18g_roots_carry_the_26a428_version_word_and_are_0xc8_bytes",
    "g18g_main_configs_use_the_26a428_views_policy_pointer_and_openings",
    "g18g_device_control_vocabulary_opens_with_0x15_and_never_emits_0x20",
    "g18g_copier_reads_land_on_fields_the_encoder_owns",
    "g18g_bands_map_every_25g76_copier_read_onto_a_b000_read",
    "g18g_hw_data_moves_every_j813_value_and_leaves_none_behind",
    "g18g_register_map_renumbers_rebases_and_zeroes_slot_41",
    "g18g_region_c_shifts_by_four_from_0x38_and_carries_the_j873_tunables",
    "g18g_private_cluster_carries_status_links_producers_and_relocated_config",
    "g18g_policy_block_is_zero_except_relocated_and_known_fields",
    "g18g_unproven_table_names_real_fields",
    "g18g_encoders_fail_closed_on_wrong_sizes_and_addresses",
)

with tempfile.TemporaryDirectory(prefix="g18-initdata-") as tmp:
    test = Path(tmp) / "test.rs"
    test.write_text(with_g17_config(
        source + '#[path = "' + str(root / "g17_initdata.rs") + '"] mod g17_initdata;\n'))
    exe = Path(tmp) / "test"
    subprocess.run(["rustc", "-Awarnings", "--edition=2021", "--test", str(test), "-o", str(exe)],
                   check=True)
    listing = subprocess.check_output([str(exe), "--list"], text=True)
    for method in G18_TESTS:
        name = "g17_initdata::g18_tests::" + method
        assert name + ": test" in listing, name
    listed = sum(1 for line in listing.splitlines() if "::g18_tests::" in line)
    assert listed == len(G18_TESTS), (listed, len(G18_TESTS))
    # Run everything in the module: the new G18G tests and the existing M5/J700 ones.
    subprocess.run([str(exe), "-q"], check=True)
    print("G18_INITDATA_OK tests=%d" % len(G18_TESTS))
