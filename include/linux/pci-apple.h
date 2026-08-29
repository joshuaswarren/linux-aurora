/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _LINUX_PCI_APPLE_H
#define _LINUX_PCI_APPLE_H

#include <linux/errno.h>
#include <linux/kconfig.h>
#include <linux/pci.h>
#include <linux/types.h>

#if IS_ENABLED(CONFIG_PCIE_APPLE)
bool apple_pcie_piodma_active(struct pci_dev *pdev);
int apple_pcie_piodma_device_read(struct pci_dev *pdev, int bar,
				  resource_size_t offset, u32 *value);
#else
static inline bool apple_pcie_piodma_active(struct pci_dev *pdev)
{
	return false;
}

static inline int apple_pcie_piodma_device_read(struct pci_dev *pdev, int bar,
					resource_size_t offset, u32 *value)
{
	return -EOPNOTSUPP;
}
#endif

#endif
