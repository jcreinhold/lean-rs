import RuntimeModel.Basic

/-!
# Pool and affine lease skeleton

This file records the small affine lease fact needed by the runtime model:
once a lease is consumed or released, it is no longer reusable.
-/

namespace RuntimeModel

/-- Abstract status of one pool lease token. -/
inductive LeaseStatus where
  | available
  | consumed
  | released
  | dropped
deriving DecidableEq, Repr

/-- Minimal pool state for affine lease reasoning. -/
structure PoolState where
  capacity : Nat
  leaseStatus : LeaseId -> LeaseStatus

/-- A lease is reusable exactly while its status is `available`. -/
def LeaseReusable (state : PoolState) (lease : LeaseId) : Prop :=
  state.leaseStatus lease = .available

/-- Mark one lease as consumed, released, or dropped. -/
def PoolState.markLease
    (state : PoolState) (lease : LeaseId) (status : LeaseStatus) : PoolState where
  capacity := state.capacity
  leaseStatus := fun candidate =>
    if candidate = lease then
      status
    else
      state.leaseStatus candidate

/-- A consumed lease is not reusable. -/
theorem consumed_lease_not_reusable
    {state : PoolState} {lease : LeaseId}
    (consumed : state.leaseStatus lease = .consumed) :
    ¬ LeaseReusable state lease := by
  intro reusable
  unfold LeaseReusable at reusable
  rw [consumed] at reusable
  contradiction

/-- A released lease is not reusable. -/
theorem released_lease_not_reusable
    {state : PoolState} {lease : LeaseId}
    (released : state.leaseStatus lease = .released) :
    ¬ LeaseReusable state lease := by
  intro reusable
  unfold LeaseReusable at reusable
  rw [released] at reusable
  contradiction

/-- A consumed or released lease is not reusable. -/
theorem consumed_or_released_lease_not_reusable
    {state : PoolState} {lease : LeaseId}
    (terminal :
      state.leaseStatus lease = .consumed ∨ state.leaseStatus lease = .released) :
    ¬ LeaseReusable state lease := by
  intro reusable
  cases terminal with
  | inl consumed =>
      exact consumed_lease_not_reusable consumed reusable
  | inr released =>
      exact released_lease_not_reusable released reusable

end RuntimeModel
