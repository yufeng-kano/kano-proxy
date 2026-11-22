import { afterEach, expect, it } from "vitest"
import { pendingConfirm, settleConfirm, useConfirm } from "@/composables/useConfirm"

// Every destructive action waits on this promise (docs/admin-ui.md § Component
// primitives): one that never settles leaves the action hanging, one that
// settles true fires it.

const options = { title: "Revoke this key?", message: "Clients using it stop working.", confirmLabel: "Revoke" }

afterEach(() => settleConfirm(false))

it("fires only on an explicit yes", async () => {
  const answer = useConfirm()(options)
  expect(pendingConfirm.value?.title).toBe(options.title)
  settleConfirm(true)
  await expect(answer).resolves.toBe(true)
  expect(pendingConfirm.value).toBeNull()
})

it("cancels the open request when another one arrives", async () => {
  const confirm = useConfirm()
  const first = confirm(options)
  const second = confirm({ ...options, title: "Remove this account?" })
  await expect(first).resolves.toBe(false)
  expect(pendingConfirm.value?.title).toBe("Remove this account?")
  settleConfirm(true)
  await expect(second).resolves.toBe(true)
})
