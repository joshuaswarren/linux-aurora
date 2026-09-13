"""Train only the J813 N1 PCIe port before the native Linux handoff.

Executed with the verified m1n1 proxy's u/p/iface scope. Does not load radio
firmware or start DMA. Linux will own the DART and endpoint driver.
"""
from m1n1.fw.smc import SMCClient
import time
import signal

assert p.get_chipid() == 0x8142 and u.adt.model == 'Mac17,3'
node = u.adt['/arm-io/apcie']
dart_node = u.adt['/arm-io/dart-apcie0']
assert dart_node.get_reg(0) == (0x488000000, 0x20000)
assert list(dart_node.interrupts) == [1463]
port_base = node.get_reg(8)[0]
print(f'N1_PCIE_PORT base={port_base:#x} phy={node.get_reg(10)[0]:#x}', flush=True)
# The resident candidate powers the root controller during startup. If a prior
# proxy probe already trained N1, retain that link and its SMC ownership.
smc = None
if not p.read32(port_base + 0x208) & (1 << 12):
    smc = SMCClient(u, u.adt['/arm-io/smc'].get_reg(0)[0], None)
    smc.verbose = 0
    smc.start()
    smc.start_ep(0x20)
    # gP13 is N1 reset. Readback is not meaningful; check the link below.
    smc.epmap[0x20].write32('gP13', 0x1000001)
    time.sleep(0.1)
result = p.pcie_init()
link = p.read32(port_base + 0x208)
print(f'N1_PCIE_INIT result={result} link={link:#x}', flush=True)
if not link & (1 << 12):
    raise RuntimeError('N1 PCIe link is down; refusing endpoint config access')
time.sleep(0.1)
ecam = 0x1cb0000000
# Limit enumeration to bus 1. Do not probe other physical root ports.
p.write32(ecam + 0x18, 0x00010100)
identity = p.read32(ecam + 0x100000)
print(f'N1_PCIE_ENDPOINT vendor={identity & 0xffff:04x} device={identity >> 16:04x}', flush=True)
if identity & 0xffff != 0x106b or identity >> 16 not in [0x1900, 0x1901]:
    raise RuntimeError('Unexpected endpoint identity on the N1 port')
if globals().get('quiesce_smc_for_linux', False):
    if smc is None:
        raise RuntimeError('No owned SMC session to hand off; use a fresh reset')
    def handoff_deadline(_signum, _frame):
        raise TimeoutError('SMC quiesce did not finish; refusing Linux handoff')
    previous_handler = signal.signal(signal.SIGALRM, handoff_deadline)
    signal.alarm(10)
    try:
        # Standard RTKit handoff, matching m1n1 smc_shutdown/rtkit_quiesce.
        # State 0x10 leaves the SMC processor running; it is not sleep/off.
        management = smc.mgmt
        smc.stop(state=0x10)
        if management.iop_power_state != 0x10 or management.ap_power_state != 0x10:
            raise RuntimeError('SMC did not acknowledge the quiesced state')
        if not p.read32(u.adt['/arm-io/smc'].get_reg(0)[0] + 0x44) & 0x10:
            raise RuntimeError('SMC processor unexpectedly stopped')
        print('M5_SMC_LINUX_HANDOFF_QUIESCED CPU_RUN=1', flush=True)
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, previous_handler)
