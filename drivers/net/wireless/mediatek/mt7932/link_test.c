// SPDX-License-Identifier: GPL-2.0-only
#include <kunit/test.h>
#include "mt7932.h"

static int mt_link_test_init(struct kunit *test)
{
	struct mt7932 *m = kunit_kzalloc(test, sizeof(*m), GFP_KERNEL);

	KUNIT_ASSERT_NOT_NULL(test, m);
	m->pdev = kunit_kzalloc(test, sizeof(*m->pdev), GFP_KERNEL);
	KUNIT_ASSERT_NOT_NULL(test, m->pdev);
	m->pdev->dev.init_name = "mt7932-link-test";
	m->netdev = kunit_kzalloc(test, sizeof(*m->netdev), GFP_KERNEL);
	KUNIT_ASSERT_NOT_NULL(test, m->netdev);
	spin_lock_init(&m->response_lock);
	init_completion(&m->assoc_update);
	init_completion(&m->assoc_done);
	m->connecting = true;
	m->connect_channel = 149;
	m->bw_limit = 20;
	test->priv = m;
	return 0;
}

static void mt_link_test_event(struct mt7932 *m, u32 selector, u16 status)
{
	u8 packet[72] = {};
	struct mt7932_event event = { .packet = packet, .length = sizeof(packet) };
	unsigned long flags;

	put_unaligned_le32(selector, packet + 40);
	put_unaligned_le16(status, packet + 48);
	put_unaligned_le32(2, packet + 60);
	packet[65] = 149;
	spin_lock_irqsave(&m->response_lock, flags);
	mt_link_event(m, &event);
	spin_unlock_irqrestore(&m->response_lock, flags);
}

static void mt_link_repeated_start(struct kunit *test)
{
	struct mt7932 *m = test->priv;
	unsigned int i;

	for (i = 0; i < 2; i++) {
		mt_link_test_event(m, 0x4d, 0);
		KUNIT_ASSERT_EQ(test, m->connect_error, 0);
		KUNIT_EXPECT_TRUE(test, try_wait_for_completion(&m->assoc_update));
		KUNIT_EXPECT_TRUE(test, m->assoc_pending);
		KUNIT_EXPECT_FALSE(test, completion_done(&m->assoc_done));
		KUNIT_EXPECT_EQ(test, m->assoc_center, (u8)149);
		/* Simulate the serialized worker finishing this calibration. */
		m->assoc_pending = false;
	}
	m->peer_valid = true;
	mt_link_test_event(m, 0x42, 0);
	KUNIT_EXPECT_EQ(test, m->connect_error, 0);
	KUNIT_EXPECT_TRUE(test, try_wait_for_completion(&m->assoc_update));
	KUNIT_EXPECT_TRUE(test, completion_done(&m->assoc_done));
}

static void mt_link_overlapping_start(struct kunit *test)
{
	struct mt7932 *m = test->priv;

	mt_link_test_event(m, 0x4d, 0);
	KUNIT_ASSERT_TRUE(test, try_wait_for_completion(&m->assoc_update));
	/* A second event cannot overwrite parameters still owned by calibration. */
	m->assoc_center = 42;
	mt_link_test_event(m, 0x4d, 0);
	KUNIT_EXPECT_EQ(test, m->connect_error, -EPROTO);
	KUNIT_EXPECT_EQ(test, m->assoc_center, (u8)42);
	KUNIT_EXPECT_TRUE(test, try_wait_for_completion(&m->assoc_update));
}

static void mt_link_terminal_wakeup(struct kunit *test)
{
	struct mt7932 *m = test->priv;

	m->peer_valid = true;
	mt_link_test_event(m, 0x42, 1);
	KUNIT_EXPECT_EQ(test, m->connect_error, -ECONNREFUSED);
	KUNIT_EXPECT_TRUE(test, try_wait_for_completion(&m->assoc_update));
	KUNIT_EXPECT_TRUE(test, completion_done(&m->assoc_done));
}

static void mt_link_start_after_done(struct kunit *test)
{
	struct mt7932 *m = test->priv;

	m->peer_valid = true;
	mt_link_test_event(m, 0x42, 0);
	KUNIT_ASSERT_EQ(test, m->connect_error, 0);
	mt_link_test_event(m, 0x4d, 0);
	KUNIT_EXPECT_EQ(test, m->connect_error, -EPROTO);
	KUNIT_EXPECT_FALSE(test, m->assoc_pending);
}

static void mt_link_malformed_report(struct kunit *test)
{
	struct mt7932 *m = test->priv;

	mt_link_test_event(m, 0x40, 0);
	KUNIT_EXPECT_EQ(test, m->connect_error, -EPROTO);
	KUNIT_EXPECT_TRUE(test, try_wait_for_completion(&m->assoc_update));
	KUNIT_EXPECT_TRUE(test, completion_done(&m->assoc_done));
}

static struct kunit_case mt_link_cases[] = {
	KUNIT_CASE(mt_link_repeated_start),
	KUNIT_CASE(mt_link_overlapping_start),
	KUNIT_CASE(mt_link_terminal_wakeup),
	KUNIT_CASE(mt_link_start_after_done),
	KUNIT_CASE(mt_link_malformed_report),
	{}
};

static struct kunit_suite mt_link_suite = {
	.name = "mt7932-link",
	.init = mt_link_test_init,
	.test_cases = mt_link_cases,
};

kunit_test_suite(mt_link_suite);

MODULE_LICENSE("GPL");
