import { match, P } from "ts-pattern"

class A {
  constructor(public foo: "bar" | "baz") {}
}

class B {
  constructor(public str: string) {}
}

export type Input = { prop: A | B }

export const inputs = [
  { prop: new A("bar") },
  { prop: new A("baz") },
  { prop: new B("hello") },
] satisfies Input[]

export const run = (input: Input): string =>
  match(input)
    .with({ prop: P.intersection(P.instanceOf(A), { foo: "bar" }) }, ({ prop }) => prop.foo)
    .with({ prop: P.intersection(P.instanceOf(A), { foo: "baz" }) }, ({ prop }) => prop.foo)
    .otherwise(() => "")
