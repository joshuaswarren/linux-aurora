// SPDX-License-Identifier: GPL-2.0-only
/* One-shot J700 diagnostic: detach the static dockchannel boot console after
 * VT and network console are available. NBCON logging threads cannot start
 * while a boot console remains. No hardware or firmware power changes.
 * No exit hook: this adjustment lasts until reboot.
 */
#include <linux/console.h>
#include <linux/module.h>
#include <linux/of.h>

static int __init neo_netconsole_prep(void)
{
	struct console *con, *boot = NULL;
	bool vt = false, net = false;
	int cookie;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	cookie = console_srcu_read_lock();
	for_each_console_srcu(con) {
		short flags = console_srcu_read_flags(con);

		if (!(flags & CON_ENABLED) || (flags & CON_SUSPENDED))
			continue;
		if (!strcmp(con->name, "tty") && con->index == 0)
			vt = true;
		if (!strcmp(con->name, "netcon_ext") && (flags & CON_NBCON))
			net = true;
		if (!strcmp(con->name, "dockchannel") && con->index == 0 &&
		    (flags & CON_BOOT))
			boot = con;
	}
	console_srcu_read_unlock(cookie);
	if (!vt || !net || !boot)
		return -EBUSY;
	/* This built-in early console has static lifetime on the pinned profile. */
	return unregister_console(boot);
}
module_init(neo_netconsole_prep);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 boot-console release for awake netconsole diagnostics");
