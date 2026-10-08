// SPDX-License-Identifier: GPL-2.0-only OR MIT
/* Copyright 2023 */

#include <linux/completion.h>

#include "afk.h"
#include "dcp.h"

static void disp_service_init(struct apple_epic_service *service, const char *name,
			const char *class, s64 unit)
{
}


/*
 * The service name follows the pipe, not the endpoint.  The internal DCP
 * offers "disp0-service" on DISP0_ENDPOINT; the external one offers
 * "dispext-service" on the same endpoint, and an unmatched service is only
 * reported as an AFK warning:
 *
 *   AFK[ep:23]: unable to match service dispext-service on channel 1
 *
 * which is easy to read as noise from an endpoint that should not have been
 * started.  Both are the same do-nothing handshake, so accept both.
 */
static const struct apple_epic_service_ops ibootep_ops[] = {
	{
		.name = "disp0-service",
		.init = disp_service_init,
	},
	{
		.name = "dispext-service",
		.init = disp_service_init,
	},
	{}
};

int ibootep_init(struct apple_dcp *dcp)
{
	dcp->ibootep = afk_init(dcp, DISP0_ENDPOINT, ibootep_ops);
	afk_start(dcp->ibootep);

	return 0;
}
