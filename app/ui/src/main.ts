// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

// Rabita Setup: the six-screen wizard for joining, and, once joined, a
// Status screen with Leave domain. Everything is done in this window; the
// desktop's own password dialog (polkit) asks for permission.
// Text the person types is only ever inserted with textContent, never as HTML.

import * as api from "./api.js";
import {
  adminProblem,
  ALL_STEPS,
  buildJoinRequest,
  buildLeaveRequest,
  buildOptions,
  checkTone,
  describeDiscovery,
  displayManagerName,
  domainProblem,
  emptyForm,
  finishView,
  formatDate,
  hostnameProblem,
  ipProblem,
  LEAVE_STEP_IDS,
  leaveProblem,
  leaveView,
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
  DnsSearch,
  JobResult,
  JoinOptions,
  StatusReport,
  StepState,
  SystemInfo,
} from "./types.js";

type ScreenList = ReadonlyArray<{ id: ScreenId; label: string }>;

const SETUP_SCREENS: ScreenList = [
  { id: "welcome", label: "Welcome" },
  { id: "organization", label: "Your organization" },
  { id: "admin", label: "IT admin sign-in" },
  { id: "options", label: "Choose options" },
  { id: "setup", label: "Setting up" },
  { id: "finish", label: "Finish" },
];

const MANAGE_SCREENS: ScreenList = [
  { id: "status", label: "Status" },
  { id: "leave", label: "Leave domain" },
  { id: "leaving", label: "Leaving" },
  { id: "left", label: "Done" },
];

type JobKind = "join" | "finish" | "leave" | "undo";

interface JobView {
  id: number | null;
  kind: JobKind;
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
  statusLoading: boolean;
  statusError: string | null;
  discovery: DiscoveryResult | null;
  discoveredFor: string;
  busy: boolean;
  error: string | null;
  dnsSearch: DnsSearch | null;
  dnsBusy: "detect" | "set" | null;
  dnsMessage: { tone: Tone; text: string } | null;
  /** A problem with the DNS address typed or found, shown inside the DNS box. */
  dnsProblem: string | null;
  /** Rabita changed the DNS server during this visit. */
  dnsChanged: boolean;
  form: FormState;
  job: JobView | null;
  showDetails: boolean;
}

const state: State = {
  screen: "welcome",
  system: null,
  systemError: null,
  status: null,
  statusLoading: false,
  statusError: null,
  discovery: null,
  discoveredFor: "",
  busy: false,
  error: null,
  dnsSearch: null,
  dnsBusy: null,
  dnsMessage: null,
  dnsProblem: null,
  dnsChanged: false,
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

function field(id: string, label: string, input: HTMLElement, hint?: string): HTMLElement {
  return h(
    "div",
    { class: "field" },
    h("label", { for: id }, label),
    input,
    hint !== undefined ? h("p", { class: "hint", id: `${id}-hint` }, hint) : null,
  );
}

function textInput(id: string, value: string, onInput: (v: string) => void, extra: Props = {}): HTMLInputElement {
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

function passwordInput(id: string, value: string, onInput: (v: string) => void): HTMLElement {
  const input = h("input", {
    id,
    type: "password",
    value,
    autocomplete: "current-password",
    oninput: (ev: Event) => onInput((ev.target as HTMLInputElement).value),
  });
  const toggle = h(
    "button",
    {
      class: "btn btn-small",
      type: "button",
      "aria-controls": id,
      onclick: (ev: Event) => {
        const show = input.type === "password";
        input.type = show ? "text" : "password";
        (ev.target as HTMLButtonElement).textContent = show ? "Hide" : "Show";
      },
    },
    "Show",
  );
  return h("div", { class: "field" }, h("label", { for: id }, "Password"), h("div", { class: "row" }, input, toggle));
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
  return state.discovery?.domain ?? state.status?.domain ?? state.system?.joined_domain ?? normaliseDomain(state.form.domain);
}

function needsRename(): boolean {
  return state.system?.hostname_problem != null;
}

// ------------------------------------------------------------------- dialog

interface DialogButton {
  label: string;
  primary?: boolean;
  action?: () => void;
}

/** A themed question with buttons. The first button gets the focus. */
function ask(title: string, body: string, buttons: DialogButton[]): void {
  const dialog = byId<HTMLDialogElement>("dialog");
  byId("dialog-title").textContent = title;
  byId("dialog-body").textContent = body;
  const row = byId("dialog-buttons");
  row.replaceChildren(
    ...buttons.map((b) =>
      h(
        "button",
        {
          class: b.primary === true ? "btn primary" : "btn",
          type: "button",
          onclick: () => {
            dialog.close();
            b.action?.();
          },
        },
        b.label,
      ),
    ),
  );
  dialog.showModal();
  (row.firstElementChild as HTMLElement | null)?.focus();
}

// ------------------------------------------------------------------ welcome

function welcomeScreen(): HTMLElement {
  const s = state.system;
  const parts: Child[] = [
    h("h1", {}, "Connect this computer to your organization"),
    h(
      "p",
      { class: "lead" },
      "Rabita sets up this computer so staff can sign in with their work username and password, even when the network is down.",
    ),
    // Make room for a problem to fix; the next screens ask for these anyway.
    !(s?.hostname_problem != null || s?.restore_pending === true) &&
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
    return h("div", {}, ...parts);
  }
  if (s === null) {
    parts.push(spinner("Checking this computer…"));
    return h("div", {}, ...parts);
  }
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
    return h("div", {}, ...parts);
  }
  if (s.joined_domain !== null) {
    // Normally the Status screen opens instead; this shows if it couldn't.
    parts.push(panel("info", `This computer is already connected to ${s.joined_domain}`));
    return h("div", {}, ...parts);
  }
  if (s.restore_pending) {
    const dns = s.dns_change;
    parts.push(
      panel(
        "info",
        "Settings from an earlier attempt",
        dns !== null
          ? `Rabita set this computer's DNS server to ${dns.server} on “${dns.connection_name}” for a connection that was not completed.`
          : "Rabita changed this computer's settings for a connection that was not completed.",
        "Carry on to finish connecting, or choose Undo changes to put the old settings back.",
      ),
    );
  }
  if (s.hostname_problem !== null) {
    parts.push(renamePanel(s));
  }
  return h("div", {}, ...parts);
}

function renamePanel(s: SystemInfo): HTMLElement {
  const problem = hostnameProblem(state.form.newHostname);
  const hint = h(
    "p",
    { class: problem === null ? "hint" : "hint hint-error", id: "new-name-hint" },
    problem ?? "Rabita renames the computer just before joining. Leaving the domain puts the old name back.",
  );
  const input = textInput(
    "new-name",
    state.form.newHostname,
    (v) => {
      state.form.newHostname = v;
      const p = hostnameProblem(v);
      hint.textContent = p ?? "Rabita renames the computer just before joining. Leaving the domain puts the old name back.";
      hint.className = p === null ? "hint" : "hint hint-error";
      renderFooter();
    },
    { "aria-describedby": "new-name-hint" },
  );
  return h(
    "div",
    { class: "panel panel-warn" },
    h("p", { class: "panel-title" }, "This computer needs a new name"),
    h("p", {}, s.hostname_problem ?? ""),
    h("div", { class: "field tight" }, h("label", { for: "new-name" }, "New name"), input, hint),
  );
}

// ------------------------------------------------------------- organization

function organizationScreen(): HTMLElement {
  const input = textInput(
    "domain",
    state.form.domain,
    (v) => {
      state.form.domain = v;
      state.error = null;
      state.dnsSearch = null;
      if (!state.dnsChanged) state.dnsMessage = null;
      refreshDiscoveryArea();
      renderFooter();
    },
    {
      placeholder: "school.example.org",
      "aria-describedby": "domain-hint",
      onkeydown: (ev: Event) => {
        if ((ev as KeyboardEvent).key === "Enter") {
          ev.preventDefault();
          void find();
        }
      },
    },
  );
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
    h("div", { id: "discovery-area" }, ...discoveryArea()),
  );
}

function discoveryArea(): Node[] {
  const out: Node[] = [];
  if (state.dnsMessage !== null) {
    out.push(panel(state.dnsMessage.tone, state.dnsMessage.text));
  }
  if (state.error !== null) {
    out.push(panel("error", state.error));
    return out;
  }
  if (state.busy) {
    out.push(spinner("Looking for your organization… this can take a few seconds."));
    return out;
  }
  const r = state.discovery;
  if (r === null || state.discoveredFor !== normaliseDomain(state.form.domain)) return out;
  const view = describeDiscovery(r);
  // Not found: one box that explains and offers the DNS fix.
  out.push(view.dnsFixable ? dnsCard(r.domain) : panel(view.tone, view.headline, ...view.lines));
  return out;
}

function refreshDiscoveryArea(): void {
  document.getElementById("discovery-area")?.replaceChildren(...discoveryArea());
}

function dnsCard(domain: string): HTMLElement {
  const busy = state.dnsBusy !== null;
  const input = textInput("dns-server", state.form.dnsServer, (v) => {
    state.form.dnsServer = v;
  }, {
    placeholder: "10.0.0.10",
    inputmode: "decimal",
    "aria-label": "DNS server IP address",
    onkeydown: (ev: Event) => {
      if ((ev as KeyboardEvent).key === "Enter") {
        ev.preventDefault();
        void useDns(state.form.dnsServer);
      }
    },
  });
  const parts: Child[] = [
    h("p", { class: "panel-title" }, `${domain} was not found`),
    h(
      "p",
      { class: "muted" },
      "Check the spelling. If it is right, this computer is probably not using your organization's DNS server. Enter its IP address, or let Rabita look for it.",
    ),
    h(
      "div",
      { class: "row" },
      input,
      h("button", { class: "btn", type: "button", id: "use-dns", disabled: busy, onclick: () => void useDns(state.form.dnsServer) }, "Use this server"),
    ),
    state.dnsProblem !== null ? h("p", { class: "hint hint-error", role: "alert" }, state.dnsProblem) : null,
  ];
  const lookLink = (label: string): HTMLElement =>
    h("button", { class: "link", type: "button", id: "detect-dns", disabled: busy, onclick: () => void detectDns() }, label);
  const search = state.dnsSearch;
  if (state.dnsBusy === "detect") {
    parts.push(spinner("Looking on this network… this takes a few seconds."));
  } else if (state.dnsBusy === "set") {
    parts.push(spinner("Changing the DNS server… enter your password if you are asked."));
  } else if (search === null) {
    parts.push(h("p", { class: "below" }, lookLink("Look for it on this network")));
  } else if (search.problem !== null) {
    parts.push(h("p", { class: "hint hint-error" }, search.problem));
  } else if (search.candidates.length === 0) {
    parts.push(h("p", { class: "below muted" }, `No DNS server on this network knows ${domain}. Ask your IT team for its address. `, lookLink("Look again")));
  } else {
    parts.push(
      h(
        "ul",
        { class: "candidates", "aria-label": `DNS servers that know ${domain}` },
        h("li", { class: "candidates-label" }, "Found:"),
        ...search.candidates.map((ip) =>
          h("li", {}, h("span", {}, ip), h("button", { class: "btn btn-small", type: "button", onclick: () => void useDns(ip) }, "Use")),
        ),
      ),
    );
  }
  return h("div", { class: "panel panel-error", id: "dns-card", role: "alert" }, ...parts);
}

async function detectDns(): Promise<void> {
  state.dnsBusy = "detect";
  state.dnsSearch = null;
  state.dnsProblem = null;
  refreshDiscoveryArea();
  try {
    state.dnsSearch = await api.findDnsServers(normaliseDomain(state.form.domain));
  } catch (e) {
    state.dnsProblem = (e as Error).message;
  } finally {
    state.dnsBusy = null;
  }
  refreshDiscoveryArea();
}

async function useDns(server: string): Promise<void> {
  const problem = ipProblem(server);
  if (problem !== null) {
    state.dnsProblem = problem;
    refreshDiscoveryArea();
    document.getElementById("dns-server")?.focus();
    return;
  }
  state.form.dnsServer = server.trim();
  state.dnsBusy = "set";
  state.dnsMessage = null;
  state.dnsProblem = null;
  refreshDiscoveryArea();
  renderFooter();
  try {
    const result = await api.setDnsServer(normaliseDomain(state.form.domain), server.trim());
    if (result.ok) {
      state.dnsMessage = { tone: "ok", text: result.message };
      state.dnsChanged = true;
      state.dnsSearch = null;
      state.dnsBusy = null;
      await find();
      return;
    }
    state.dnsProblem = result.message;
  } catch (e) {
    state.dnsProblem = (e as Error).message;
  } finally {
    state.dnsBusy = null;
  }
  refreshDiscoveryArea();
  renderFooter();
}

// ------------------------------------------------------- admin and options

function adminScreen(): HTMLElement {
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
      textInput(
        "admin-user",
        state.form.adminUser,
        (v) => {
          state.form.adminUser = v;
          renderFooter();
        },
        { autocomplete: "username", "aria-describedby": "admin-user-hint" },
      ),
      "Just the user name, for example joiner.",
    ),
    passwordInput("password", state.form.password, (v) => {
      state.form.password = v;
      renderFooter();
    }),
    h(
      "details",
      { class: "advanced" },
      h("summary", {}, "Advanced"),
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

// -------------------------------------------------------- progress (jobs)

function isLeaveKind(kind: JobKind): boolean {
  return kind === "leave" || kind === "undo";
}

function jobTitle(job: JobView): string {
  switch (job.kind) {
    case "join":
      return "Connecting this computer…";
    case "finish":
      return "Finishing setup…";
    case "leave":
      return `Leaving ${job.domain}…`;
    case "undo":
      return "Putting back the earlier settings…";
  }
}

function stepIcon(s: StepState): HTMLElement {
  return h("span", { class: `step-icon ${s}`, "aria-hidden": "true" });
}

function progressScreen(): HTMLElement {
  const job = state.job;
  if (job === null) return h("div", {});
  const leave = isLeaveKind(job.kind);
  const list = h("ol", { class: "checklist" });
  for (const id of job.stepIds) {
    const st = job.steps.get(id) ?? { state: "pending" as StepState, message: "" };
    list.append(
      h(
        "li",
        { "data-step": id, class: st.state },
        stepIcon(st.state),
        h("div", {}, h("span", { class: "step-label" }, stepLabel(id, leave)), h("span", { class: "step-msg", title: st.message }, st.message)),
      ),
    );
  }
  const fraction = progressFraction(job.stepIds.map((id) => job.steps.get(id)?.state ?? "pending"));
  const bar = h("div", { class: "bar", id: "bar" });
  bar.style.width = `${Math.round(fraction * 100)}%`;
  return h(
    "div",
    {},
    h("h1", {}, jobTitle(job)),
    h(
      "p",
      { class: "lead" },
      leave ? "This takes a minute. Please keep this window open." : "This can take a few minutes while software is installed. Please keep this window open.",
    ),
    h(
      "div",
      {
        class: "progress",
        id: "progress",
        role: "progressbar",
        "aria-label": "Progress",
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

function resultIcon(tone: Tone): SVGElement {
  if (tone === "ok") {
    return svg(
      "svg",
      { class: "result-icon ok", viewBox: "0 0 64 64", "aria-hidden": "true" },
      svg("circle", { cx: "32", cy: "32", r: "29" }),
      svg("path", { class: "tick", d: "M19 33 L28 42 L46 23" }),
    );
  }
  return svg(
    "svg",
    { class: `result-icon ${tone}`, viewBox: "0 0 64 64", "aria-hidden": "true" },
    svg("circle", { cx: "32", cy: "32", r: "29" }),
    tone === "warn"
      ? svg("path", { class: "mark", d: "M32 18 L32 36 M32 45 L32 46" })
      : svg("path", { class: "mark", d: "M23 23 L41 41 M41 23 L23 41" }),
  );
}

function finishScreen(): HTMLElement {
  const job = state.job;
  if (job?.result == null) return h("div", {});
  const view = finishView(job.result, job.domain);
  const parts: Child[] = [h("div", { class: "result" }, resultIcon(view.tone), h("h1", {}, view.title)), h("p", { class: "lead" }, view.body)];
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

// ------------------------------------------------------------ status/leave

function statusScreen(): HTMLElement {
  const st = state.status;
  const domain = domainName();
  if (st === null) {
    return h(
      "div",
      {},
      h("h1", {}, `Connected to ${domain}`),
      state.statusError !== null
        ? panel("error", "Rabita Setup couldn't check the connection.", state.statusError)
        : spinner("Checking the connection…"),
    );
  }
  const unfinished = st.setup_finished === false;
  const parts: Child[] = [
    h("h1", {}, unfinished ? `Connected to ${domain}, but setup is not finished` : `Connected to ${domain}`),
    h(
      "p",
      { class: "lead" },
      unfinished
        ? `Still to do: ${st.remaining_steps.map((id) => stepLabel(id)).join(", ")}. Choose Finish setup to complete it.`
        : "Staff can sign in here with their work username and password.",
    ),
    h(
      "dl",
      { class: "facts" },
      h("dt", {}, "Computer name"),
      h("dd", {}, st.computer_name ?? state.system?.hostname ?? "Not known"),
      h("dt", {}, "Domain controller"),
      h("dd", {}, st.domain_controller ?? "Not known"),
      h("dt", {}, "Connected since"),
      h("dd", {}, formatDate(st.joined_at)),
    ),
    h("p", { class: "section" }, "Health checks", state.statusLoading ? h("span", { class: "spin small", "aria-label": "Checking" }) : null),
    h(
      "ul",
      { class: "checks" },
      ...st.checks.map((c) =>
        h(
          "li",
          { class: `check-${checkTone(c)}` },
          h("span", { class: "check-icon", "aria-hidden": "true" }),
          h("div", {}, h("span", { class: "check-label" }, c.label), h("span", { class: "check-detail", title: c.detail }, c.detail)),
        ),
      ),
    ),
  ];
  if (state.statusError !== null) parts.push(panel("error", state.statusError));
  return h("div", { class: "status-screen" }, ...parts);
}

function leaveScreen(): HTMLElement {
  const f = state.form;
  const domain = domainName();
  const account = state.status?.computer_name ?? state.system?.hostname?.toUpperCase() ?? "this computer";
  const credentials = h(
    "div",
    { class: "indent", hidden: !f.removeAccount, id: "leave-credentials" },
    field(
      "leave-user",
      "Admin user name",
      textInput(
        "leave-user",
        f.leaveUser,
        (v) => {
          f.leaveUser = v;
          renderFooter();
        },
        { autocomplete: "username", "aria-describedby": "leave-user-hint" },
      ),
      "An IT admin account that is allowed to remove computers.",
    ),
    passwordInput("leave-password", f.leavePassword, (v) => {
      f.leavePassword = v;
      renderFooter();
    }),
  );
  const localNote = h(
    "div",
    { class: "indent", hidden: f.removeAccount, id: "leave-local-note" },
    panel("info", "The computer account stays in the directory", `Ask your IT admin to delete ${account} in Active Directory Users and Computers afterwards.`),
  );
  return h(
    "div",
    {},
    h("h1", {}, `Leave ${domain}`),
    h(
      "p",
      { class: "lead" },
      "After leaving, staff can no longer sign in to this computer with their work accounts. Their files stay on this computer. Rabita puts back the settings it changed: sign-in, login screen, DNS server and computer name.",
    ),
    checkbox("remove-account", `Also remove this computer from ${domain} (recommended)`, f.removeAccount, (v) => {
      f.removeAccount = v;
      credentials.hidden = !v;
      localNote.hidden = v;
      state.error = null;
      renderFooter();
    }),
    credentials,
    localNote,
    h("p", { class: "muted small" }, `If ${domain} can't be reached, Rabita leaves on this computer only and tells you what to do next.`),
    errorLine(),
  );
}

function leftScreen(): HTMLElement {
  const job = state.job;
  if (job?.result == null) return h("div", {});
  const view = leaveView(job.result, job.domain, job.kind === "undo");
  const parts: Child[] = [h("div", { class: "result" }, resultIcon(view.tone), h("h1", {}, view.title)), h("p", { class: "lead" }, view.body)];
  if (job.result.ok && job.kind === "leave") {
    parts.push(
      h(
        "div",
        { class: "card" },
        h("p", { class: "card-title" }, "What happens next"),
        h("p", { class: "muted" }, "Sign out or restart this computer so the login screen goes back to normal."),
      ),
    );
  }
  if (!job.result.ok) parts.push(detailsToggle());
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

function button(label: string, action: () => void, primary = false, enabled = true): ButtonSpec {
  return { label, enabled, visible: true, primary, action };
}

function footerSpec(): { back: ButtonSpec; next: ButtonSpec; extra: ButtonSpec; cancel: ButtonSpec } {
  const idx = SETUP_SCREENS.findIndex((s) => s.id === state.screen);
  const back = button("‹ Back", goBack, false, idx > 0 && !state.busy);
  const next = button("Next ›", goNext, true, false);
  const cancel = button("Cancel", askCancel, false, !state.busy);
  let extra = hidden();
  switch (state.screen) {
    case "welcome": {
      const s = state.system;
      next.enabled =
        s !== null &&
        s.supported &&
        s.joined_domain === null &&
        (!needsRename() || hostnameProblem(state.form.newHostname) === null);
      if (s?.restore_pending === true && s.joined_domain === null) {
        extra = button("Undo changes", () => void startUndo(), false, !state.busy);
      }
      break;
    }
    case "organization": {
      const r = state.discovery;
      next.enabled =
        !state.busy &&
        state.dnsBusy === null &&
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
    case "leaving":
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
        extra = button("Finish setup", () => void startFinish(buildOptions(state.form), state.job?.domain ?? domainName()), true);
      }
      break;
    }
    case "status": {
      const st = state.status;
      const unfinished = st?.setup_finished === false && st.options !== null;
      Object.assign(back, button("Refresh", () => void refreshStatus(), false, !state.statusLoading && !state.busy));
      extra = button("Leave domain…", () => go("leave"), false, !state.busy);
      if (unfinished && st?.options != null) {
        const options = st.options;
        Object.assign(next, button("Finish setup", () => void startFinish(options, domainName()), true, !state.busy));
        Object.assign(cancel, button("Close", () => void api.quit()));
      } else {
        Object.assign(next, button("Close", () => void api.quit(), true));
        cancel.visible = false;
      }
      break;
    }
    case "leave":
      Object.assign(back, button("‹ Back", () => go("status"), false, !state.busy));
      Object.assign(next, button("Leave domain", confirmLeave, true, !state.busy && leaveProblem(state.form) === null));
      Object.assign(cancel, button("Cancel", () => go("status"), false, !state.busy));
      break;
    case "left": {
      const job = state.job;
      const view = job?.result != null ? leaveView(job.result, job.domain, job.kind === "undo") : null;
      back.visible = view?.backTo != null;
      back.enabled = back.visible;
      back.action = () => {
        if (view?.backTo != null) {
          state.job = null;
          go(view.backTo);
        }
      };
      if (view?.canRetry === true) extra = button("Try again", () => void startUndo(), false);
      Object.assign(next, button("Close", () => void api.quit(), true));
      cancel.visible = false;
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

function bannerScreens(): ScreenList {
  return MANAGE_SCREENS.some((s) => s.id === state.screen) ? MANAGE_SCREENS : SETUP_SCREENS;
}

function renderBanner(): void {
  const list = byId<HTMLOListElement>("steps");
  const screens = bannerScreens();
  const idx = screens.findIndex((s) => s.id === state.screen);
  list.replaceChildren(
    ...screens.map((s, i) =>
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
    case "leaving":
      return progressScreen();
    case "finish":
      return finishScreen();
    case "status":
      return statusScreen();
    case "leave":
      return leaveScreen();
    case "left":
      return leftScreen();
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
  if (target !== null && target.offsetParent !== null) {
    target.focus();
    return;
  }
  const next = byId<HTMLButtonElement>("next");
  const extra = byId<HTMLButtonElement>("extra");
  (!next.hidden && !next.disabled ? next : !extra.hidden && !extra.disabled ? extra : next).focus();
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
  const idx = SETUP_SCREENS.findIndex((s) => s.id === state.screen);
  const prev = SETUP_SCREENS[idx - 1];
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
    case "admin":
      if (adminProblem(state.form) === null) go("options");
      break;
    case "options":
      void startJoin();
      break;
    default:
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
  document.getElementById("find")?.setAttribute("disabled", "");
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
  else document.getElementById("dns-server")?.focus() ?? document.getElementById("domain")?.focus();
}

function newJob(kind: JobKind, domain: string): JobView {
  const stepIds = kind === "join" ? ALL_STEPS : kind === "finish" ? POST_JOIN_STEPS : LEAVE_STEP_IDS;
  state.showDetails = false;
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

/** Starts a job and moves to its progress screen; errors return to `from`. */
async function runJob(kind: JobKind, domain: string, start: () => Promise<number>, onStarted?: () => void): Promise<void> {
  const from = state.screen;
  state.busy = true;
  state.error = null;
  renderFooter();
  state.job = newJob(kind, domain);
  try {
    // The desktop may show its password dialog (polkit) here.
    const id = await start();
    onStarted?.();
    if (state.job !== null) state.job.id = id;
    state.busy = false;
    if (state.job?.result == null) go(isLeaveKind(kind) ? "leaving" : "setup");
  } catch (e) {
    state.busy = false;
    state.job = null;
    state.screen = from;
    state.error = (e as Error).message;
    render();
    // The admin-options and leave screens show state.error themselves.
    if (from !== "options" && from !== "leave") byId<HTMLElement>("content").append(panel("error", state.error));
  }
}

function startJoin(): Promise<void> {
  const problem = optionsProblem(state.form);
  if (problem !== null) {
    state.error = problem;
    render();
    return Promise.resolve();
  }
  const request = buildJoinRequest(state.form, needsRename());
  return runJob("join", request.domain, () => api.join(request, state.form.password), () => {
    // The job has started: the admin password is no longer needed.
    state.form.password = "";
  });
}

function startFinish(options: JoinOptions, domain: string): Promise<void> {
  return runJob("finish", domain, () => api.applyOptions(options));
}

function confirmLeave(): void {
  const domain = domainName();
  ask(
    `Leave ${domain}?`,
    "People will no longer be able to sign in to this computer with their work accounts. Their files stay on this computer.",
    [{ label: "Keep connected" }, { label: "Leave", primary: true, action: () => void startLeave() }],
  );
}

function startLeave(): Promise<void> {
  const request = buildLeaveRequest(state.form);
  const password = state.form.removeAccount ? state.form.leavePassword : "";
  return runJob("leave", domainName(), () => api.leave(request, password), () => {
    state.form.leavePassword = "";
  });
}

function startUndo(): Promise<void> {
  return runJob("undo", state.system?.dns_change?.domain ?? domainName(), () =>
    api.leave({ remove_computer_account: false, admin_user: null }, ""),
  );
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
  // The computer's state changed: later screens should see the new facts.
  void refreshSystem();
  if (isLeaveKind(job.kind)) state.dnsChanged = false;
  const target: ScreenId = isLeaveKind(job.kind) ? "left" : "finish";
  // Let the last tick show before moving on.
  const onProgressScreen = state.screen === "setup" || state.screen === "leaving";
  window.setTimeout(() => go(target), onProgressScreen ? 700 : 0);
}

// ---------------------------------------------------------------- cancel

function askCancel(): void {
  if (state.screen === "welcome" || state.screen === "finish" || state.screen === "status" || state.screen === "left") {
    void api.quit();
    return;
  }
  if (state.screen === "leave") {
    go("status");
    return;
  }
  if (state.dnsChanged) {
    ask("Close Rabita Setup?", "Rabita changed this computer's DNS server. Put the old setting back before closing?", [
      { label: "Keep going" },
      { label: "Keep the change", action: () => void api.quit() },
      { label: "Undo and close", primary: true, action: () => void undoDnsAndQuit() },
    ]);
    return;
  }
  ask("Close Rabita Setup?", "Nothing on this computer has been changed yet.", [
    { label: "Keep going" },
    { label: "Close", primary: true, action: () => void api.quit() },
  ]);
}

async function undoDnsAndQuit(): Promise<void> {
  try {
    const result = await api.restoreDns();
    if (result.ok) {
      await api.quit();
      return;
    }
    ask("The DNS setting was not put back", result.message, [{ label: "OK" }]);
  } catch (e) {
    ask("The DNS setting was not put back", (e as Error).message, [{ label: "OK" }]);
  }
}

// ------------------------------------------------------------------ start

async function refreshSystem(): Promise<void> {
  try {
    state.system = await api.detectSystem();
    state.systemError = null;
    if (state.form.newHostname === "" && state.system.suggested_hostname !== null) {
      state.form.newHostname = state.system.suggested_hostname;
    }
  } catch (e) {
    state.systemError = (e as Error).message;
  }
}

async function refreshStatus(): Promise<void> {
  state.statusLoading = true;
  state.statusError = null;
  if (state.screen === "status") render();
  try {
    state.status = await api.status();
  } catch (e) {
    state.statusError = (e as Error).message;
  } finally {
    state.statusLoading = false;
  }
  if (state.screen === "status") render();
}

async function start(): Promise<void> {
  document.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && !byId<HTMLDialogElement>("dialog").open) {
      const cancel = byId<HTMLButtonElement>("cancel");
      if (!cancel.hidden && !cancel.disabled) {
        // Open after this key press is done, or the same Escape closes the dialog again.
        ev.preventDefault();
        window.setTimeout(askCancel, 0);
      }
    }
    const t = ev.target;
    if (ev.key === "Enter" && t instanceof HTMLInputElement && !["domain", "dns-server"].includes(t.id) && t.type !== "checkbox" && t.type !== "radio") {
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
  if (state.screen !== "welcome") return;
  if (state.system?.joined_domain != null) {
    // Already joined: open on the Status screen.
    state.screen = "status";
    render(true);
    await refreshStatus();
    focusFirst();
    return;
  }
  render();
  focusFirst();
}

void start();
