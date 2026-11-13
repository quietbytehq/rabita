// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

// Talks to the Rust side of the app, which talks to rabita-helper over
// D-Bus. Uses the global Tauri API (`app.withGlobalTauri`), so no npm
// runtime packages are needed.

import type {
  DiscoveryResult,
  JobResult,
  JoinOptions,
  JoinRequest,
  StatusReport,
  StepState,
  SystemInfo,
} from "./types.js";

interface TauriEvent<T> {
  payload: T;
}

interface TauriGlobal {
  core: { invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> };
  event: { listen<T>(event: string, handler: (e: TauriEvent<T>) => void): Promise<() => void> };
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobal;
  }
}

function tauri(): TauriGlobal {
  const t = window.__TAURI__;
  if (t === undefined) {
    throw new Error("Rabita Setup must be opened as an app, not in a web browser.");
  }
  return t;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await tauri().core.invoke<T>(cmd, args);
  } catch (e) {
    throw new Error(typeof e === "string" ? e : e instanceof Error ? e.message : String(e));
  }
}

async function callJson<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return JSON.parse(await call<string>(cmd, args)) as T;
}

export function detectSystem(): Promise<SystemInfo> {
  return callJson<SystemInfo>("detect_system");
}

export function discoverDomain(domain: string): Promise<DiscoveryResult> {
  return callJson<DiscoveryResult>("discover_domain", { domain });
}

export function status(): Promise<StatusReport> {
  return callJson<StatusReport>("status");
}

/** Starts a join and returns its job id. Progress arrives as events. */
export function join(request: JoinRequest, password: string): Promise<number> {
  return call<number>("join", { request: JSON.stringify(request), password });
}

/** Re-applies options, or finishes a setup that stopped after joining. */
export function applyOptions(options: JoinOptions): Promise<number> {
  return call<number>("apply_options", { options: JSON.stringify(options) });
}

export function quit(): Promise<void> {
  return call<void>("quit");
}

export interface ProgressEvent {
  job: number;
  step: string;
  state: StepState;
  message: string;
}

export interface LogEvent {
  job: number;
  line: string;
}

export interface FinishedEvent {
  job: number;
  ok: boolean;
  result: JobResult;
}

interface FinishedPayload {
  job: number;
  ok: boolean;
  result_json: string;
}

export function onProgress(cb: (e: ProgressEvent) => void): Promise<() => void> {
  return tauri().event.listen<ProgressEvent>("rabita:progress", (e) => cb(e.payload));
}

export function onLog(cb: (e: LogEvent) => void): Promise<() => void> {
  return tauri().event.listen<LogEvent>("rabita:log", (e) => cb(e.payload));
}

export function onFinished(cb: (e: FinishedEvent) => void): Promise<() => void> {
  return tauri().event.listen<FinishedPayload>("rabita:finished", (e) => {
    const p = e.payload;
    let result: JobResult;
    try {
      result = JSON.parse(p.result_json) as JobResult;
    } catch {
      result = {
        ok: p.ok,
        summary: p.ok ? "Done." : "The helper reported a problem but its details could not be read.",
        steps: [],
        joined_not_finished: false,
        remaining_steps: [],
      };
    }
    cb({ job: p.job, ok: p.ok, result });
  });
}
