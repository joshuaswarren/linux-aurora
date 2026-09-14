// SPDX-License-Identifier: GPL-2.0-only
/* Diagnostic noirq endpoint snapshots; never changes interrupt routing. */
#include <linux/cpu.h>
#include <linux/debugfs.h>
#include <linux/interrupt.h>
#include <linux/irq.h>
#include <linux/irqdesc.h>
#include <linux/ktime.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/platform_device.h>
#include <linux/seq_file.h>

#define MAX_IRQS 256
static unsigned long before[MAX_IRQS], after[MAX_IRQS];
static u64 begin_ns, end_ns;
static unsigned int cycles;
static struct platform_device *pdev;
static struct dentry *root;

static void snapshot(unsigned long *counts)
{
	unsigned int irq, cpu;

	/* Match this module to the running kernel's irq_desc layout. RCU keeps
	 * descriptors alive; noirq also excludes driver removal and reallocation.
	 * Counters may advance on another CPU while an endpoint is sampled.
	 */
	rcu_read_lock();
	for (irq = 0; irq < MAX_IRQS; irq++) {
		struct irq_data *data = irq_get_irq_data(irq);
		struct irq_desc *desc;
		unsigned long total = 0;

		if (!data) {
			counts[irq] = 0;
			continue;
		}
		desc = irq_data_to_desc(data);
		if (desc->kstat_irqs)
			for_each_possible_cpu(cpu)
				total += READ_ONCE(per_cpu_ptr(desc->kstat_irqs, cpu)->cnt);
		counts[irq] = total;
	}
	rcu_read_unlock();
}

static int sleep_noirq(struct device *dev)
{
	end_ns = 0;
	snapshot(before);
	begin_ns = ktime_get_boottime_ns();
	return 0;
}

static int wake_noirq(struct device *dev)
{
	end_ns = ktime_get_boottime_ns();
	snapshot(after);
	cycles++;
	return 0;
}

static int stats_show(struct seq_file *s, void *unused)
{
	unsigned int irq;

	seq_printf(s, "cycles %u begin_ns %llu end_ns %llu max_irq %u\n",
		   cycles, begin_ns, end_ns, MAX_IRQS - 1);
	for (irq = 0; irq < MAX_IRQS; irq++)
		if (before[irq] || after[irq])
			seq_printf(s, "%u %lu %lu\n", irq, before[irq], after[irq]);
	return 0;
}
DEFINE_SHOW_ATTRIBUTE(stats);

static int irq_probe(struct platform_device *dev) { return 0; }
static const struct dev_pm_ops irq_pm = {
	.suspend_noirq = sleep_noirq,
	.resume_noirq = wake_noirq,
};
static struct platform_driver irq_driver = {
	.probe = irq_probe,
	.driver = { .name = "neo-sleep-irqs", .pm = &irq_pm },
};

static int __init irq_init(void)
{
	int ret;

	if (!of_machine_is_compatible("apple,j700"))
		return -ENODEV;
	if (irq_get_nr_irqs() > MAX_IRQS)
		return -E2BIG;
	ret = platform_driver_register(&irq_driver);
	if (ret)
		return ret;
	pdev = platform_device_register_simple("neo-sleep-irqs", -1, NULL, 0);
	if (IS_ERR(pdev)) {
		ret = PTR_ERR(pdev);
		goto unregister;
	}
	root = debugfs_create_dir("neo-sleep-irqs", NULL);
	if (IS_ERR(root)) {
		ret = PTR_ERR(root);
		platform_device_unregister(pdev);
		goto unregister;
	}
	debugfs_create_file("stats", 0400, root, NULL, &stats_fops);
	return 0;
unregister:
	platform_driver_unregister(&irq_driver);
	return ret;
}
static void __exit irq_exit(void)
{
	debugfs_remove_recursive(root);
	platform_device_unregister(pdev);
	platform_driver_unregister(&irq_driver);
}
module_init(irq_init);
module_exit(irq_exit);
MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("J700 read-only IRQ counts across noirq sleep callbacks");
