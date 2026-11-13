// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

// The Rabita Setup wizard: six screens, classic installer layout.
// Text the person types is only ever inserted with textContent, never as HTML.

import * as api from "./api.js";
import {
  adminProblem,
  ALL_STEPS,
  buildJoinRequest,
  buildOptions,
  describeDiscovery,
  displayManagerName,
  domainProblem,
  emptyForm,
  finishView,
  normaliseDomain,
  optionsProblem,
  POST_JOIN_STEPS,
  progressFraction,
  stepLabel,
  type FormState,
  type ScreenId,
  type Tone,
} from "./logic.js";
import type {
  DiscoveryResult,
  JobResult,
  JoinOptions,
  StatusReport,
  StepState,
  SystemInfo,
} from "./types.js";

const SCREENS: ReadonlyArray<{ id: ScreenId; label: string }> = [
  { id: "welcome", label: "Welcome" },
  { id: "organization", label: "Your organization" },
  { id: "admin", label: "IT admin sign-in" },
  { id: "options", label: "Choose options" },
  { id: "setup", label: "Setting up" },
  { id: "finish", label: "Finish" },
];

interface JobView {
  id: number | null;
  kind: "join" | "finish";
  domain: string;
  stepIds: readonly string[];
  steps: Map<string, { state: StepState; message: string }>;
  log: string[];
  result: JobResult | null;
}

interface State {
  screen: ScreenId;
  system: SystemInfo | null;
  systemError: string | null;
  status: StatusReport | null;
  discovery: DiscoveryResult | null;
  discoveredFor: string;
  busy: boolean;
  error: string | null;
  form: FormState;
  job: JobView | null;
  showDetails: boolean;
}

const state: State = {
  screen: "welcome",
  system: null,
  systemError: null,
  status: null,
  discovery: null,
  discoveredFor: "",
  busy: false,
  error: null,
  form: emptyForm(),
  job: null,
  showDetails: false,
};

// ---------------------------------------------------------------- DOM helpers

type Child = Node | string | null | undefined | false;
type Props = Record<string, string | boolean | ((ev: Event) => void) | undefined>;

function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Props = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === false) continue;
    if (typeof value === "function") {
      el.addEventListener(key.replace(/^on/, ""), value);
    } else if (key === "value" && (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) {
      el.value = String(value);
    } else if (key === "checked" && el instanceof HTMLInputElement) {
      el.checked = value === true;
    } else {
      el.setAttribute(key, value === true ? "" : value);
    }
  }
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    el.append(typeof c === "string" ? document.createTextNode(c) : c);
  }
  return el;
}

const SVG_NS = "http://www.w3.org/2000/svg";

function svg(tag: string, attrs: Record<string, string>, ...children: SVGElement[]): SVGElement {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  el.append(...children);
  return el;
}

function byId<T extends HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (el === null) throw new Error(`missing #${id}`);
  return el as T;
}

function panel(tone: Tone | "info", title: string, ...lines: Child[]): HTMLElement {
  return h(
    "div",
    { class: `panel panel-${tone}`, role: tone === "error" ? "alert" : "status" },
    h("p", { class: "panel-title" }, title),
    ...lines.map((l) => (typeof l === "string" ? h("p", {}, l) : l)),
  );
}

function spinner(text: string): HTMLElement {
  return h("p", { class: "working", role: "status" }, h("span", { class: "spin", "aria-hidden": "true" }), text);
}

function field(
  id: string,
  label: string,
  input: HTMLElement,
  hint?: string,
): HTMLElement {
  return h(
    "div",
    { class: "field" },
    h("label", { for: id }, label),
    input,
    hint !== undefined ? h("p", { class: "hint", id: `${id}-hint` }, hint) : null,
  );
}

function textInput(
  id: string,
  value: string,
  onInput: (v: string) => void,
  extra: Props = {},
): HTMLInputElement {
  return h("input", {
    id,
    type: "text",
    value,
    autocomplete: "off",
    spellcheck: "false",
    autocapitalize: "off",
    oninput: (ev: Event) => onInput((ev.target as HTMLInputElement).value),
    ...extra,
  });
}

function checkbox(id: string, label: string, checked: boolean, onChange: (v: boolean) => void): HTMLElement {
  return h(
    "label",
    { class: "check", for: id },
    h("input", {
      id,
      type: "checkbox",
      checked,
      onchange: (ev: Event) => onChange((ev.target as HTMLInputElement).checked),
    }),
    h("span", { class: "box", "aria-hidden": "true" }),
    h("span", {}, label),
  );
}

function errorLine(): HTMLElement | null {
  return state.error === null ? null : panel("error", state.error);
}

function domainName(): string {
  return state.discovery?.domain ?? state.status?.domain ?? normaliseDomain(state.form.domain);
}

// ------------------------------------------------------------------- screens

function welcomeScreen(): HTMLElement {
  const s = state.system;
  const parts: Child[] = [
    h("h1", {}, "Connect this computer to your organization"),
    h(
      "p",
      { class: "lead" },
      "Rabita sets up this computer so staff can sign in with their work username and password, even when the network is down.",
    ),
    h(
      "div",
      { class: "card" },
      h("p", { class: "card-title" }, "You will need"),
      h(
        "ul",
        { class: "plain" },
        h("li", {}, "Your organization's domain name, for example school.example.org"),
        h("li", {}, "An IT admin account that is allowed to add computers"),
      ),
    ),
  ];
  if (state.systemError !== null) {
    parts.push(panel("error", "Rabita Setup can't check this computer.", state.systemError));
  } else if (s === null) {
    parts.push(spinner("Checking this computer…"));
  } else {
    parts.push(
      h(
        "dl",
        { class: "facts" },
        h("dt", {}, "This computer"),
        h("dd", {}, s.hostname),
        h("dt", {}, "System"),
        h("dd", {}, s.distro_name || s.distro_id),
        h("dt", {}, "Login screen"),
        h("dd", {}, displayManagerName(s.display_manager)),
      ),
    );
    if (!s.supported) {
      parts.push(
        panel(
          "warn",
          "Not supported yet",
          s.unsupported_reason ?? "Rabita can't set up this system yet.",
          "Nothing on this computer has been changed.",
        ),
      );
    } else if (s.joined_domain !== null) {
      const st = state.status;
      if (st !== null && st.setup_finished === false) {
        parts.push(
          panel(
            "warn",
            `Connected to ${s.joined_domain}, but setup is not finished`,
            `Still to do: ${st.remaining_steps.map(stepLabel).join(", ")}.`,
            "Choose Finish setup to complete it.",
          ),
        );
      } else {
        parts.push(
          panel(
            "info",
            `This computer is already connected to ${s.joined_domain}`,
            "This version of Rabita Setup can't change an existing connection yet.",
          ),
        );
      }
    }
  }
  return h("div", {}, ...parts);
}

function organizationScreen(): HTMLElement {
  const input = textInput("domain", state.form.domain, (v) => {
    state.form.domain = v;
    state.error = null;
    refreshDiscoveryArea();
    renderFooter();
  }, {
    placeholder: "school.example.org",
    "aria-describedby": "domain-hint",
    onkeydown: (ev: Event) => {
      if ((ev as KeyboardEvent).key === "Enter") {
        ev.preventDefault();
        void find();
      }
    },
  });
  const findButton = h(
    "button",
    { class: "btn", type: "button", id: "find", onclick: () => void find(), disabled: state.busy },
    "Find",
  );
  return h(
    "div",
    {},
    h("h1", {}, "Your organization"),
    h(
      "p",
      { class: "lead" },
      "Enter the domain name your IT team uses for staff sign-in. It usually looks like a web address.",
    ),
    h(
      "div",
      { class: "field" },
      h("label", { for: "domain" }, "Domain name"),
      h("div", { class: "row" }, input, findButton),
      h("p", { class: "hint", id: "domain-hint" }, "The full name with dots, not the short name (like SCHOOL)."),
    ),
    h("div", { id: "discovery-area" }, discoveryArea()),
  );
}

function discoveryArea(): Child {
  if (state.error !== null) return panel("error", state.error);
  if (state.busy) return spinner("Looking for your organization… this can take a few seconds.");
  const r = state.discovery;
  if (r === null || state.discoveredFor !== normaliseDomain(state.form.domain)) return null;
  const view = describeDiscovery(r);
  return panel(view.tone, view.headline, ...view.lines);
}

function refreshDiscoveryArea(): void {
  const area = document.getElementById("discovery-area");
  if (area !== null) area.replaceChildren(...[discoveryArea()].filter((c): c is Node | string => !!c));
}

function adminScreen(): HTMLElement {
  const password = h("input", {
    id: "password",
    type: "password",
    value: state.form.password,
    autocomplete: "current-password",
    oninput: (ev: Event) => {
      state.form.password = (ev.target as HTMLInputElement).value;
      renderFooter();
    },
  });
  const toggle = h(
    "button",
    {
      class: "btn btn-small",
      type: "button",
      "aria-controls": "password",
      onclick: (ev: Event) => {
        const show = password.type === "password";
        password.type = show ? "text" : "password";
        (ev.target as HTMLButtonElement).textContent = show ? "Hide" : "Show";
      },
    },
    "Show",
  );
  const host = state.system?.hostname ?? "this computer";
  return h(
    "div",
    {},
    h("h1", {}, "IT admin sign-in"),
    h(
      "p",
      { class: "lead" },
      `Sign in with an account that is allowed to add computers to ${domainName()}. The password is used once to add this computer and is never saved.`,
    ),
    field(
      "admin-user",
      "Admin user name",
      textInput("admin-user", state.form.adminUser, (v) => {
        state.form.adminUser = v;
        renderFooter();
      }, { autocomplete: "username", "aria-describedby": "admin-user-hint" }),
      "Just the user name, for example joiner.",
    ),
    h("div", { class: "field" }, h("label", { for: "password" }, "Password"), h("div", { class: "row" }, password, toggle)),
    h(
      "details",
      { class: "advanced" },
      h("summary", {}, "Advanced"),
      field(
        "computer-name",
        "Computer name",
        textInput("computer-name", state.form.computerName, (v) => (state.form.computerName = v), {
          placeholder: host.split(".")[0]?.toUpperCase() ?? "",
          "aria-describedby": "computer-name-hint",
        }),
        `Leave empty to use this computer's name (${host}).`,
      ),
      field(
        "computer-ou",
        "Organizational unit (OU)",
        textInput("computer-ou", state.form.computerOu, (v) => (state.form.computerOu = v), {
          placeholder: "OU=Linux PCs,DC=school,DC=example,DC=org",
          "aria-describedby": "computer-ou-hint",
        }),
        "Where to put this computer in the directory. Leave empty for the default.",
      ),
    ),
  );
}

function optionsScreen(): HTMLElement {
  const f = state.form;
  const groups = h("textarea", {
    id: "groups",
    rows: "2",
    value: f.groupsText,
    placeholder: "Linux Users\nStaff",
    disabled: f.accessMode !== "groups",
    "aria-label": "Groups allowed to sign in, one per line",
    oninput: (ev: Event) => {
      f.groupsText = (ev.target as HTMLTextAreaElement).value;
      state.error = null;
      renderFooter();
    },
  });
  const groupBox = h("div", { class: "indent", hidden: f.accessMode !== "groups" }, groups);
  const radio = (value: "all" | "groups", label: string): HTMLElement =>
    h(
      "label",
      { class: "check", for: `access-${value}` },
      h("input", {
        id: `access-${value}`,
        type: "radio",
        name: "access",
        checked: f.accessMode === value,
        onchange: () => {
          f.accessMode = value;
          groups.disabled = value !== "groups";
          groupBox.hidden = value !== "groups";
          if (value === "groups") groups.focus();
          state.error = null;
          renderFooter();
        },
      }),
      h("span", { class: "box round", "aria-hidden": "true" }),
      h("span", {}, label),
    );
  return h(
    "div",
    {},
    h("h1", {}, "Choose options"),
    h("p", { class: "lead" }, "The usual choices are already selected."),
    h(
      "fieldset",
      {},
      h("legend", {}, "Who can sign in"),
      radio("all", `Everyone in ${domainName()}`),
      radio("groups", "Only members of these groups (one per line)"),
      groupBox,
    ),
    h(
      "fieldset",
      {},
      h("legend", {}, "Sign-in"),
      checkbox("manual-login", "Show a box on the login screen to type a username", f.manualLogin, (v) => (f.manualLogin = v)),
      checkbox("create-home", "Create a home folder the first time someone signs in", f.createHome, (v) => (f.createHome = v)),
      checkbox("offline-login", "Let people sign in when the network is down", f.offlineLogin, (v) => (f.offlineLogin = v)),
    ),
    field(
      "verify-user",
      "Test user (optional)",
      textInput("verify-user", f.verifyUser, (v) => (f.verifyUser = v), { "aria-describedby": "verify-user-hint" }),
      "After setup, Rabita checks it can find this user.",
    ),
    errorLine(),
  );
}

function stepIcon(s: StepState): HTMLElement {
  return h("span", { class: `step-icon ${s}`, "aria-hidden": "true" });
}

function setupScreen(): HTMLElement {
  const job = state.job;
  if (job === null) return h("div", {});
  const list = h("ol", { class: "checklist" });
  for (const id of job.stepIds) {
    const st = job.steps.get(id) ?? { state: "pending" as StepState, message: "" };
    list.append(
      h(
        "li",
        { "data-step": id, class: st.state },
        stepIcon(st.state),
        h("div", {}, h("span", { class: "step-label" }, stepLabel(id)), h("span", { class: "step-msg", title: st.message }, st.message)),
      ),
    );
  }
  const fraction = progressFraction(job.stepIds.map((id) => job.steps.get(id)?.state ?? "pending"));
  const bar = h("div", { class: "bar", id: "bar" });
  bar.style.width = `${Math.round(fraction * 100)}%`;
  return h(
    "div",
    {},
    h("h1", {}, job.kind === "join" ? "Connecting this computer…" : "Finishing setup…"),
    h("p", { class: "lead" }, "This can take a few minutes while software is installed. Please keep this window open."),
    h(
      "div",
      {
        class: "progress",
        id: "progress",
        role: "progressbar",
        "aria-label": "Setup progress",
        "aria-valuemin": "0",
        "aria-valuemax": "100",
        "aria-valuenow": String(Math.round(fraction * 100)),
      },
      bar,
    ),
    list,
    detailsToggle(),
  );
}

function detailsToggle(): HTMLElement {
  const log = h("pre", { class: "log", id: "log", tabindex: "0", hidden: !state.showDetails }, (state.job?.log ?? []).join("\n"));
  const link = h(
    "button",
    {
      class: "link",
      type: "button",
      "aria-controls": "log",
      "aria-expanded": state.showDetails ? "true" : "false",
      onclick: (ev: Event) => {
        state.showDetails = !state.showDetails;
        log.hidden = !state.showDetails;
        const b = ev.target as HTMLButtonElement;
        b.textContent = state.showDetails ? "Hide details" : "Show details";
        b.setAttribute("aria-expanded", state.showDetails ? "true" : "false");
        log.scrollTop = log.scrollHeight;
      },
    },
    state.showDetails ? "Hide details" : "Show details",
  );
  return h("div", { class: "details" }, link, log);
}

function finishScreen(): HTMLElement {
  const job = state.job;
  if (job?.result == null) return h("div", {});
  const view = finishView(job.result, job.domain);
  const icon =
    view.tone === "ok"
      ? svg(
          "svg",
          { class: "result-icon ok", viewBox: "0 0 64 64", "aria-hidden": "true" },
          svg("circle", { cx: "32", cy: "32", r: "29" }),
          svg("path", { class: "tick", d: "M19 33 L28 42 L46 23" }),
        )
      : svg(
          "svg",
          { class: `result-icon ${view.tone}`, viewBox: "0 0 64 64", "aria-hidden": "true" },
          svg("circle", { cx: "32", cy: "32", r: "29" }),
          view.tone === "warn"
            ? svg("path", { class: "mark", d: "M32 18 L32 36 M32 45 L32 46" })
            : svg("path", { class: "mark", d: "M23 23 L41 41 M41 23 L23 41" }),
        );
  const parts: Child[] = [h("div", { class: "result" }, icon, h("h1", {}, view.title)), h("p", { class: "lead" }, view.body)];
  if (view.tone === "ok" && job.kind === "join") {
    const who = state.form.verifyUser.trim();
    parts.push(
      h(
        "div",
        { class: "card" },
        h("p", { class: "card-title" }, "What happens next"),
        h(
          "ol",
          { class: "plain numbered" },
          h("li", {}, "Sign out of this computer."),
          h("li", {}, who === "" ? "Sign in with a work username and password." : `Sign in with a work username, for example ${who}, and its password.`),
          h("li", {}, "The first sign-in can take a little longer while the home folder is created."),
        ),
      ),
    );
  }
  if (view.problem !== null) {
    parts.push(
      h(
        "div",
        { class: "card" },
        h("p", { class: "card-title" }, "What went wrong"),
        h("p", { class: "problem" }, view.problem),
        view.remaining.length > 0 ? h("p", { class: "card-title still" }, "Still to do") : null,
        view.remaining.length > 0 ? h("ul", { class: "plain" }, ...view.remaining.map((r) => h("li", {}, r))) : null,
      ),
    );
  }
  if (view.canFinishSetup) {
    parts.push(h("p", { class: "muted" }, "Fix the problem if you can, then choose Finish setup. Only the steps above are repeated."));
  }
  if (view.tone !== "ok") parts.push(detailsToggle());
  return h("div", {}, ...parts);
}

// -------------------------------------------------------------------- footer

interface ButtonSpec {
  label: string;
  enabled: boolean;
  visible: boolean;
  primary: boolean;
  action: () => void;
}

function hidden(): ButtonSpec {
  return { label: "", enabled: false, visible: false, primary: false, action: () => undefined };
}

function footerSpec(): { back: ButtonSpec; next: ButtonSpec; extra: ButtonSpec; cancel: ButtonSpec } {
  const idx = SCREENS.findIndex((s) => s.id === state.screen);
  const back: ButtonSpec = { label: "‹ Back", enabled: idx > 0 && !state.busy, visible: true, primary: false, action: goBack };
  const next: ButtonSpec = { label: "Next ›", enabled: false, visible: true, primary: true, action: goNext };
  const cancel: ButtonSpec = { label: "Cancel", enabled: !state.busy, visible: true, primary: false, action: askCancel };
  let extra = hidden();
  switch (state.screen) {
    case "welcome": {
      const s = state.system;
      next.enabled = s !== null && s.supported && s.joined_domain === null;
      if (s?.joined_domain != null && state.status?.setup_finished === false && state.status.options !== null) {
        const options = state.status.options;
        extra = { label: "Finish setup", enabled: true, visible: true, primary: true, action: () => void startFinish(options, s.joined_domain ?? "") };
        next.visible = false;
      }
      break;
    }
    case "organization": {
      const r = state.discovery;
      next.enabled =
        !state.busy &&
        r !== null &&
        state.discoveredFor === normaliseDomain(state.form.domain) &&
        describeDiscovery(r).canContinue;
      break;
    }
    case "admin":
      next.enabled = adminProblem(state.form) === null;
      break;
    case "options":
      next.label = "Connect";
      next.enabled = !state.busy && optionsProblem(state.form) === null;
      break;
    case "setup":
      back.enabled = false;
      next.enabled = false;
      cancel.enabled = false;
      break;
    case "finish": {
      const result = state.job?.result;
      const view = result != null ? finishView(result, state.job?.domain ?? "") : null;
      back.visible = view?.backTo != null;
      back.enabled = back.visible;
      next.label = "Close";
      next.enabled = true;
      next.primary = view?.canFinishSetup !== true;
      next.action = () => void api.quit();
      cancel.visible = false;
      if (view?.canFinishSetup === true) {
        extra = {
          label: "Finish setup",
          enabled: true,
          visible: true,
          primary: true,
          action: () => void startFinish(buildOptions(state.form), state.job?.domain ?? domainName()),
        };
      }
      break;
    }
  }
  return { back, next, extra, cancel };
}

function applyButton(el: HTMLButtonElement, spec: ButtonSpec): void {
  el.hidden = !spec.visible;
  el.disabled = !spec.enabled;
  el.textContent = spec.label;
  el.classList.toggle("primary", spec.primary);
  el.onclick = () => {
    if (!el.disabled) spec.action();
  };
}

function renderFooter(): void {
  const spec = footerSpec();
  applyButton(byId<HTMLButtonElement>("back"), spec.back);
  applyButton(byId<HTMLButtonElement>("next"), spec.next);
  applyButton(byId<HTMLButtonElement>("extra"), spec.extra);
  applyButton(byId<HTMLButtonElement>("cancel"), spec.cancel);
}

// ---------------------------------------------------------------- rendering

function renderBanner(): void {
  const list = byId<HTMLOListElement>("steps");
  const idx = SCREENS.findIndex((s) => s.id === state.screen);
  list.replaceChildren(
    ...SCREENS.map((s, i) =>
      h(
        "li",
        { class: i < idx ? "done" : i === idx ? "active" : "todo", "aria-current": i === idx ? "step" : undefined },
        h("span", { class: "dot", "aria-hidden": "true" }),
        s.label,
      ),
    ),
  );
}

function screenElement(): HTMLElement {
  switch (state.screen) {
    case "welcome":
      return welcomeScreen();
    case "organization":
      return organizationScreen();
    case "admin":
      return adminScreen();
    case "options":
      return optionsScreen();
    case "setup":
      return setupScreen();
    case "finish":
      return finishScreen();
  }
}

/** Re-renders the whole screen. `animate` plays the slide-in. */
function render(animate = false): void {
  renderBanner();
  const content = byId<HTMLElement>("content");
  const el = screenElement();
  el.classList.add("screen");
  if (animate) el.classList.add("enter");
  content.replaceChildren(el);
  renderFooter();
}

function focusFirst(): void {
  const content = byId<HTMLElement>("content");
  const target = content.querySelector<HTMLElement>("input:not([disabled]), textarea:not([disabled])");
  if (target !== null) {
    target.focus();
  } else {
    const next = byId<HTMLButtonElement>("next");
    const extra = byId<HTMLButtonElement>("extra");
    (!extra.hidden && !extra.disabled ? extra : next).focus();
  }
}

function go(screen: ScreenId): void {
  state.screen = screen;
  state.error = null;
  render(true);
  focusFirst();
}

// --------------------------------------------------------------- navigation

function goBack(): void {
  if (state.screen === "finish") {
    const result = state.job?.result;
    const target = result != null ? finishView(result, state.job?.domain ?? "").backTo : null;
    if (target !== null) {
      state.job = null;
      go(target);
    }
    return;
  }
  const idx = SCREENS.findIndex((s) => s.id === state.screen);
  const prev = SCREENS[idx - 1];
  if (prev !== undefined) go(prev.id);
}

function goNext(): void {
  switch (state.screen) {
    case "welcome":
      go("organization");
      break;
    case "organization":
      go("admin");
      break;
    case "admin": {
      const problem = adminProblem(state.form);
      if (problem === null) go("options");
      break;
    }
    case "options":
      void startJoin();
      break;
    case "setup":
    case "finish":
      break;
  }
}

async function find(): Promise<void> {
  const problem = domainProblem(state.form.domain);
  if (problem !== null) {
    state.error = problem;
    refreshDiscoveryArea();
    renderFooter();
    return;
  }
  const domain = normaliseDomain(state.form.domain);
  state.busy = true;
  state.error = null;
  refreshDiscoveryArea();
  renderFooter();
  byId<HTMLButtonElement>("find").disabled = true;
  try {
    state.discovery = await api.discoverDomain(domain);
    state.discoveredFor = domain;
  } catch (e) {
    state.error = (e as Error).message;
  } finally {
    state.busy = false;
  }
  if (state.screen !== "organization") return;
  render();
  const next = byId<HTMLButtonElement>("next");
  if (!next.disabled) next.focus();
  else document.getElementById("domain")?.focus();
}

function newJob(kind: JobView["kind"], domain: string): JobView {
  const stepIds = kind === "join" ? ALL_STEPS : POST_JOIN_STEPS;
  return {
    id: null,
    kind,
    domain,
    stepIds,
    steps: new Map(stepIds.map((id) => [id, { state: "pending" as StepState, message: "" }])),
    log: [],
    result: null,
  };
}

async function startJoin(): Promise<void> {
  const problem = optionsProblem(state.form);
  if (problem !== null) {
    state.error = problem;
    render();
    return;
  }
  const request = buildJoinRequest(state.form);
  state.busy = true;
  state.error = null;
  renderFooter();
  state.job = newJob("join", request.domain);
  try {
    // The helper asks for the computer's own admin password (polkit) here.
    const id = await api.join(request, state.form.password);
    // The job has started: the admin password is no longer needed.
    state.form.password = "";
    if (state.job !== null) state.job.id = id;
    state.busy = false;
    if (state.job?.result == null) go("setup");
  } catch (e) {
    state.busy = false;
    state.job = null;
    state.error = (e as Error).message;
    render();
  }
}

async function startFinish(options: JoinOptions, domain: string): Promise<void> {
  state.busy = true;
  renderFooter();
  state.job = newJob("finish", domain);
  try {
    const id = await api.applyOptions(options);
    if (state.job !== null) state.job.id = id;
    state.busy = false;
    if (state.job?.result == null) go("setup");
  } catch (e) {
    state.busy = false;
    state.job = null;
    state.error = (e as Error).message;
    render();
    const content = byId<HTMLElement>("content");
    content.append(panel("error", state.error));
  }
}

// ------------------------------------------------------------ job events

function belongsToJob(jobId: number): boolean {
  const job = state.job;
  // Events can arrive before the job id does; there is only ever one job.
  return job !== null && job.result === null && (job.id === null || job.id === jobId);
}

function onProgress(e: api.ProgressEvent): void {
  if (!belongsToJob(e.job) || state.job === null) return;
  const job = state.job;
  job.steps.set(e.step, { state: e.state, message: e.message });
  const row = document.querySelector<HTMLLIElement>(`li[data-step="${CSS.escape(e.step)}"]`);
  if (row !== null) {
    row.className = e.state;
    row.querySelector(".step-icon")?.setAttribute("class", `step-icon ${e.state}`);
    const msg = row.querySelector(".step-msg");
    if (msg !== null) {
      msg.textContent = e.message;
      msg.setAttribute("title", e.message);
    }
    if (e.state === "running") row.scrollIntoView({ block: "nearest" });
  }
  const fraction = Math.round(progressFraction(job.stepIds.map((id) => job.steps.get(id)?.state ?? "pending")) * 100);
  const bar = document.getElementById("bar");
  if (bar !== null) bar.style.width = `${fraction}%`;
  document.getElementById("progress")?.setAttribute("aria-valuenow", String(fraction));
}

function onLog(e: api.LogEvent): void {
  if (!belongsToJob(e.job) || state.job === null) return;
  state.job.log.push(e.line);
  const log = document.getElementById("log");
  if (log !== null) {
    log.textContent = state.job.log.join("\n");
    log.scrollTop = log.scrollHeight;
  }
}

function onFinished(e: api.FinishedEvent): void {
  if (!belongsToJob(e.job) || state.job === null) return;
  const job = state.job;
  job.result = e.result;
  for (const s of e.result.steps) job.steps.set(s.id, { state: s.state, message: s.message });
  if (e.result.ok || e.result.joined_not_finished) {
    // The computer is now in the domain, so later visits show its status.
    void refreshSystem();
  }
  // Let the last tick show before moving on.
  window.setTimeout(() => go("finish"), state.screen === "setup" ? 700 : 0);
}

// ---------------------------------------------------------------- cancel

function askCancel(): void {
  if (state.screen === "welcome" || state.screen === "finish") {
    void api.quit();
    return;
  }
  const dialog = byId<HTMLDialogElement>("confirm");
  dialog.showModal();
}

function setupDialog(): void {
  const dialog = byId<HTMLDialogElement>("confirm");
  byId<HTMLButtonElement>("confirm-stay").onclick = () => dialog.close();
  byId<HTMLButtonElement>("confirm-quit").onclick = () => void api.quit();
}

// ------------------------------------------------------------------ start

async function refreshSystem(): Promise<void> {
  try {
    state.system = await api.detectSystem();
    state.systemError = null;
    if (state.system.joined_domain !== null) {
      state.status = await api.status();
    }
  } catch (e) {
    state.systemError = (e as Error).message;
  }
}

async function start(): Promise<void> {
  setupDialog();
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && !byId<HTMLDialogElement>("confirm").open) {
      const cancel = byId<HTMLButtonElement>("cancel");
      if (!cancel.hidden && !cancel.disabled) {
        // Open after this key press is done, or the same Escape closes the dialog again.
        ev.preventDefault();
        window.setTimeout(askCancel, 0);
      }
    }
    if (ev.key === "Enter" && ev.target instanceof HTMLInputElement && ev.target.id !== "domain" && ev.target.type !== "checkbox" && ev.target.type !== "radio") {
      const next = byId<HTMLButtonElement>("next");
      if (!next.hidden && !next.disabled) {
        ev.preventDefault();
        next.click();
      }
    }
  });
  render(true);
  try {
    await Promise.all([api.onProgress(onProgress), api.onLog(onLog), api.onFinished(onFinished)]);
  } catch (e) {
    state.systemError = (e as Error).message;
  }
  await refreshSystem();
  if (state.screen === "welcome") {
    render();
    focusFirst();
  }
}

void start();
