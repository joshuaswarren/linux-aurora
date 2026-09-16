// SPDX-License-Identifier: GPL-2.0-only
/* J700-only read-only idle configuration diagnostic, never a power driver.
 * Run only with kernel.panic_on_oops=0: the die notifier intercepts an
 * unsupported MRS at our exact instruction sites in process context.
 * An intercepted UNDEF still emits an Oops header and taints the kernel.
 * The notifier is unregistered before module init returns. No MSR or MMIO.
 */
#include <linux/cpu.h>
#include <linux/kdebug.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/percpu.h>
#include <asm/esr.h>
#include <asm/ptrace.h>
#include <asm/sysreg.h>

static bool experimental;
module_param(experimental, bool, 0400);
static int cpu = -1;
module_param(cpu, int, 0400);
static unsigned int mask = 1;
module_param(mask, uint, 0400);
MODULE_PARM_DESC(mask, "Read mask: 1 CYC_OVRD, 2 ACC_CFG, 4 ACC_OVRD, 8 MDSCR, 16 PMCR0, 32 PMCR1, 64 DBGPRCR, 128 OSLSR, 256 AMX_CTL");
static DEFINE_PER_CPU(bool, faulted);
static DEFINE_PER_CPU(bool, probing);

#define DECLARE_READER(name) \
	extern u64 neo_read_##name(void); \
	extern char neo_site_##name[]
DECLARE_READER(cyc);
DECLARE_READER(acc);
DECLARE_READER(override);
DECLARE_READER(debug);
DECLARE_READER(pmcr0);
DECLARE_READER(pmcr1);
DECLARE_READER(dbgpower);
DECLARE_READER(oslock);
DECLARE_READER(amx);

static const struct {
	const char *name;
	u64 (*read)(void);
	const char *site;
} readers[] = {
	{ "CYC_OVRD", neo_read_cyc, neo_site_cyc },
	{ "ACC_CFG", neo_read_acc, neo_site_acc },
	{ "ACC_OVRD", neo_read_override, neo_site_override },
	{ "MDSCR", neo_read_debug, neo_site_debug },
	{ "PMCR0", neo_read_pmcr0, neo_site_pmcr0 },
	{ "PMCR1", neo_read_pmcr1, neo_site_pmcr1 },
	{ "DBGPRCR", neo_read_dbgpower, neo_site_dbgpower },
	{ "OSLSR", neo_read_oslock, neo_site_oslock },
	{ "AMX_CTL", neo_read_amx, neo_site_amx },
};

static int neo_undef(struct notifier_block *nb, unsigned long reason, void *data)
{
	struct die_args *a = data;
	unsigned int i;

	if (reason != DIE_OOPS || !a->regs || user_mode(a->regs) ||
	    in_interrupt() || ESR_ELx_EC(a->err) != ESR_ELx_EC_UNKNOWN ||
	    !this_cpu_read(probing))
		return NOTIFY_DONE;
	for (i = 0; i < ARRAY_SIZE(readers); i++) {
		if (a->regs->pc != (unsigned long)readers[i].site)
			continue;
		this_cpu_write(faulted, true);
		a->regs->regs[0] = 0;
		a->regs->pc += 4;
		return NOTIFY_STOP;
	}
	return NOTIFY_DONE;
}

static struct notifier_block neo_notifier = {
	.notifier_call = neo_undef,
	.priority = INT_MAX,
};

static long neo_snapshot(void *unused)
{
	unsigned long flags;
	u64 values[ARRAY_SIZE(readers)] = {};
	bool failed[ARRAY_SIZE(readers)] = {};
	unsigned int i;
	int id = smp_processor_id();
	u64 mpidr = read_sysreg(mpidr_el1), midr = read_sysreg(midr_el1);

	/* work_on_cpu uses process context; never probe from a cross-CPU IPI. */
	local_irq_save(flags);
	this_cpu_write(probing, true);
	for (i = 0; i < ARRAY_SIZE(readers); i++) {
		if (!(mask & BIT(i)))
			continue;
		this_cpu_write(faulted, false);
		values[i] = readers[i].read();
		failed[i] = this_cpu_read(faulted);
	}
	this_cpu_write(probing, false);
	local_irq_restore(flags);
	pr_info("neo-cpu-regs cpu=%d mpidr=%#llx midr=%#llx\n", id, mpidr, midr);
	for (i = 0; i < ARRAY_SIZE(readers); i++)
		if (mask & BIT(i))
			pr_info("neo-cpu-regs cpu=%d %s=%#018llx status=%s\n",
				id, readers[i].name, values[i], failed[i] ? "UNDEF" : "read");
	return 0;
}

static int __init neo_init(void)
{
	int c, ret;

	if (!experimental || !of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	if (!mask || mask & ~GENMASK(ARRAY_SIZE(readers) - 1, 0) || cpu < -1 ||
	    (cpu >= 0 && cpu >= nr_cpu_ids))
		return -EINVAL;
	cpus_read_lock();
	if (cpu >= 0 && !cpu_online(cpu)) {
		ret = -ENODEV;
		goto unlock;
	}
	ret = register_die_notifier(&neo_notifier);
	if (ret)
		goto unlock;
	for_each_online_cpu(c) {
		if (cpu >= 0 && c != cpu)
			continue;
		ret = work_on_cpu(c, neo_snapshot, NULL);
		if (ret)
			break;
	}
	unregister_die_notifier(&neo_notifier);
unlock:
	cpus_read_unlock();
	return ret;
}

static void __exit neo_exit(void) {}
module_init(neo_init);
module_exit(neo_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Read-only J700 per-core idle configuration diagnostic");
