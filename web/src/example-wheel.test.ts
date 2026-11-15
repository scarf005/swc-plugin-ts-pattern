import { assert, test } from "vite-plus/test"
import { cycleValue, getWheelStep } from "./example-wheel.ts"

test("getWheelStep uses dominant wheel axis", () => {
  assert.deepEqual(getWheelStep({ deltaX: 0, deltaY: 120 }), 1)
  assert.deepEqual(getWheelStep({ deltaX: 0, deltaY: -120 }), -1)
  assert.deepEqual(getWheelStep({ deltaX: 80, deltaY: 10 }), 1)
  assert.deepEqual(getWheelStep({ deltaX: -80, deltaY: 10 }), -1)
  assert.deepEqual(getWheelStep({ deltaX: 0, deltaY: 0 }), 0)
})

test("cycleValue wraps around options", () => {
  const values = ["a", "b", "c"] as const
  assert.deepEqual(cycleValue(values, "a", 1), "b")
  assert.deepEqual(cycleValue(values, "a", -1), "c")
  assert.deepEqual(cycleValue(values, "c", 1), "a")
  assert.deepEqual(cycleValue(values, "x", 1), undefined)
})
