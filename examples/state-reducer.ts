import { match, P } from "ts-pattern"

export type State =
  | { status: "idle" }
  | { status: "loading"; startTime: number }
  | { status: "success"; data: string }
  | { status: "error"; error: Error }

export type Event =
  | { type: "fetch" }
  | { type: "success"; data: string }
  | { type: "error"; error: Error }
  | { type: "cancel" }

export const inputs = [
  [{ status: "idle" }, { type: "fetch" }],
  [
    { status: "loading", startTime: 0 },
    { type: "success", data: "ok" },
  ],
  [
    { status: "loading", startTime: 0 },
    {
      type: "error",
      error: new Error("boom"),
    },
  ],
  [{ status: "loading", startTime: 0 }, { type: "cancel" }],
] satisfies [State, Event][]

export const run = ([state, event]: [State, Event]): State =>
  match<[State, Event], State>([state, event])
    .with([{ status: "loading" }, { type: "success" }], ([, next]) => ({
      status: "success",
      data: next.data,
    }))
    .with([{ status: "loading" }, { type: "error", error: P.select() }], (error) => ({
      status: "error",
      error,
    }))
    .with([{ status: P.not("loading") }, { type: "fetch" }], () => ({
      status: "loading",
      startTime: 0,
    }))
    .with(
      [
        { status: "loading", startTime: P.when((t) => t <= Date.now()) },
        {
          type: "cancel",
        },
      ],
      () => ({ status: "idle" }),
    )
    .with(P._, () => state)
    .exhaustive()
