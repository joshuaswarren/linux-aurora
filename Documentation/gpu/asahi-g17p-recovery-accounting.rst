G17P recovery and submit refusal accounting
==========================================

Recovery settlement uses the same guilty QID set as victim classification.
When execution evidence reattributes firmware blame, the original QID's
innocent owner is not settled as guilty. Unresolved ownership retains the
conservative firmware blame. A recovery with no culprit settles no guilty
render. Guilty grace and collateral classifications continue to spare their
owners.

Spared render work is inspected on recovery and subsequent teardown turns,
including work listed without the firmware CANCELLED flag. A real completion
retains its normal result. Cancelled work receives ECANCELED only after the
existing retirement transaction proves both queue cursors, list unlink and,
for shared work-state storage, final completion stamps and a closed recovery.
That transaction retires exactly one tracker and its FList references before
promoting a successor. No ownership proof means no cancellation, quarantine,
release or fabricated successful result. The ordinary watchdog remains the
bounded fallback for work whose retirement cannot be proved.

Context teardown
----------------

Closing a GEM handle can defer its GPUVM unmap behind accepted jobs. The
native context registry must remain valid for those jobs until the actual
unmap; removing it when close merely parks the object makes already accepted
jobs lose their context view. Registry removal now follows the actual GEM
unmap, including deferred drains.

A later submit may still arrive after userspace has removed its context BO.
A healthy G17P submit checks whether canonical view 0 is unbound, with the mapping pin held and before publication.
Such a request is refused with ENOENT and its output fence reports ENOENT.
It does not poison the VM or consume a firmware queue. A successful bind or rebind clears the unbound flag. The alias validator
also tags an exact missing-view-0 refusal that races this preflight; ioctl
and error-fence boundaries preserve ENOENT. Initialization refusals and
teardown refusals share this counter. A poisoned VM, missing views 1..6,
and ENOENT from any other submit stage retain ordinary failure reporting.
The refusal says that context teardown occurred; it does not assert that the
process is exiting rather than intentionally unbinding its context.

Counters and qualification
--------------------------

The root-readable, read-only ``g17p_queue_slots`` module parameter appends::

    submit failures=N context-unbound-refusals=M

``failures`` counts exactly the queue-submit errors logged as ``submit
failed!`` by the submit ioctl. ERESTARTSYS and the precisely identified
context-unbound refusal are excluded. Failures before queue submission
(such as an unknown queue ID or malformed ioctl flags) do not enter this
counter, matching the scope of the existing journal qualification counter.
``context-unbound-refusals`` counts the separately logged ``submit refused:
context-unbound`` events. Both counters start at module initialization,
are monotonic, and have no reset operation. Snapshot them before and after
a gate, together with kernel release and boot ID. The existing ENOSPC pool,
reserve and reclaim counters retain their semantics.

A boot containing submit ENOSPC or an increase in ``submit failures`` remains
invalid. A context-unbound refusal is a failed ioctl, not evidence of a GPU
fault; record its separate counter without treating it as a successful
submission. Unexpected quarantine, poison, GPU fault or timeout remains a
reason to stop the gate. An intentional single-fault recovery test must name
its expected guilty client and reject collateral slot retirement, timeout or
VM poison. Never loop fault injection to obtain a passing result.
