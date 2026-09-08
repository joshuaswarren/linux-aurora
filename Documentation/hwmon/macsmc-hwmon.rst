.. SPDX-License-Identifier: GPL-2.0-only

Kernel driver macsmc-hwmon
==========================

Supported hardware

    * Apple Silicon Macs (M1 and up)

Author: James Calligeros <jcalligeros99@gmail.com>

Description
-----------

macsmc-hwmon exposes the Apple System Management controller's
temperature, voltage, current and power sensors, as well as
fan speed and control capabilities, via hwmon.

Because each Apple Silicon Mac exposes a different set of sensors
(e.g. the MacBooks expose battery telemetry that is not present on
the desktop Macs), sensors present on any given machine are described
via Devicetree. The driver picks these up and registers them with
hwmon when probed.

Manual fan speed is supported via the fan_control module parameter. This
is disabled by default and marked as unsafe, as it cannot be proven that
the system will fail safe if overheating due to manual fan control being
used.

sysfs interface
---------------

currX_input
    Ammeter value

currX_label
    Ammeter label

fanX_input
    Current fan speed

fanX_label
    Fan label

fanX_min
    Minimum possible fan speed

fanX_max
    Maximum possible fan speed

fanX_target
    Current fan setpoint

inX_input
    Voltmeter value

inX_label
    Voltmeter label

powerX_input
    Power meter value

powerX_label
    Power meter label

tempX_input
    Temperature sensor value

tempX_label
    Temperature sensor label

J700 CPU thermal policy
----------------------

On J700, the optional ``apple,cpu-thermal-policy`` Devicetree property and
``CONFIG_APPLE_SMC_CPU_THERMAL=y`` couple six selected SMC readings to the
``apple-smc-selected`` thermal zone and both cpufreq cooling devices. Higher
T8140 OPP policies install persistent minimum-frequency guards before their
governors start, and cannot release them without a working provider, cooling
bindings and three complete valid rounds. Disabling the zone or removing the
provider retains those caps. Ordinary standalone hwmon reporting is unchanged.

The selected keys are Te05, Te0S, Tp01, Tp05, Tp09 and Tp0D. Labels are key
identities, not a recovered map of physical core hotspots. The policy starts
cooling at 65 C and releases below 60 C. These are Linux operating choices,
not Apple emergency thresholds. Retained firmware protection is not modified.

One serialized sampler publishes a complete six-key round to both hwmon and
thermal. Failed reads and host rounds at least 200 ms old return errors and
retain minimum CPU caps. A separate worker detects a stalled sampler without
waiting for its SMC lock. This bounds host transaction age, not sensor age:
J700 load traces showed roughly one-second plateaus in successful replies.
Faster polling cannot guarantee newer firmware samples or a hard thermal
ceiling. Higher-state admission must therefore remain separately bounded and
tested; complete table discovery alone is insufficient.
