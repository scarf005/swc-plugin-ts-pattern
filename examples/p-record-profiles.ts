import { match, P } from "ts-pattern"

export const inputs = [
  { alice: { name: "Alice", age: 25 }, bob: { name: "Bob", age: 30 } },
  { invalid: { name: "Alice", age: "old" } },
] as const

export const run = (input: Record<string, unknown>): string =>
  match(input)
    .with(P.record({ name: P.string, age: P.number }), () => "User profiles with name and age")
    .otherwise(() => "Different format")
