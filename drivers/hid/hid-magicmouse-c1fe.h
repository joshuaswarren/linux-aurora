/* SPDX-License-Identifier: GPL-2.0-or-later */
#ifndef _HID_MAGICMOUSE_C1FE_H
#define _HID_MAGICMOUSE_C1FE_H

#include <linux/types.h>
#include <linux/unaligned.h>

#define MAGICMOUSE_C1FE_HEADER_SIZE 32
#define MAGICMOUSE_C1FE_SUFFIX_SIZE 8
#define MAGICMOUSE_C1FE_REPORT_ID 0x75

static inline bool
magicmouse_c1fe_layout_valid(const u8 *data, int size, unsigned int max_contacts,
			     size_t contact_size)
{
	unsigned int count;

	if (size < MAGICMOUSE_C1FE_HEADER_SIZE ||
	    data[0] != MAGICMOUSE_C1FE_REPORT_ID ||
	    data[2] != MAGICMOUSE_C1FE_HEADER_SIZE || data[3] != 4)
		return false;

	count = data[22];
	return count <= max_contacts &&
		get_unaligned_le32(data + 16) == count * contact_size &&
		get_unaligned_le16(data + 20) == MAGICMOUSE_C1FE_SUFFIX_SIZE &&
		size == MAGICMOUSE_C1FE_HEADER_SIZE + count * contact_size +
			MAGICMOUSE_C1FE_SUFFIX_SIZE;
}

#endif /* _HID_MAGICMOUSE_C1FE_H */
