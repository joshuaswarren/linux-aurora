/* SPDX-License-Identifier: GPL-2.0-only */
#ifndef AURORA_CENTAURI_RECOVERY_H
#define AURORA_CENTAURI_RECOVERY_H

#include "centauri_t6050.h"

int centauri_t6050_boot_phase_for_control_stage(u32 control_stage,
						enum centauri_t6050_boot_phase *phase);

void centauri_t6050_schedule_transport_recover(struct centauri_t6050 *c);

#endif
