# SPDX-License-Identifier: GPL-2.0-only
obj-$(CONFIG_MT7932_FULLMAC) += mt7932-fullmac.o
mt7932-fullmac-y := mt7932.o recovery.o calibration_transport.o data.o frontend.o link.o startup.o diagnostics.o bandwidth.o
obj-$(CONFIG_MT7932_RATE_KUNIT_TEST) += rate_test.o
mt7932-fullmac-$(CONFIG_MT7932_BW_KUNIT_TEST) += bandwidth_test.o
mt7932-fullmac-$(CONFIG_MT7932_LINK_KUNIT_TEST) += link_test.o
