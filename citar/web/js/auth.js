// Sign in, sign up, password reset, and the session the rest of the app reads.
//
// The session is fetched once at startup and cached. Everything else asks `session()` rather than
// hitting /api/auth/me again, so a page render does not cost a round trip per component.
//
// Local mode signs the operator in automatically, so these screens never appear on a laptop. They
// are what the public server shows.
import { api, setCsrfToken, NotSignedIn } from "./api.js";
import { el, clear, toast } from "./util.js";

let state = null;      // { authenticated, user, capabilities, csrf_token, local_mode }
let config = null;     // { registration, providers, captcha, email_enabled, ... }

export function session() { return state; }
export function authConfig() { return config; }
export function user() { return state && state.user; }
export function can(capability) {
  return !!(state && state.capabilities && state.capabilities[capability]);
}
export function isAdmin() { return !!(state && state.user && state.user.role === "admin"); }
export function isModerator() {
  return !!(state && state.user && ["admin", "moderator"].includes(state.user.role));
}

/** The browser's IANA zone, so availability windows can be shown in the viewer's own time. */
export function browserTimezone() {
  try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC"; }
  catch (e) { return "UTC"; }
}

export async function refresh() {
  state = await api.me();
  if (state && state.csrf_token) setCsrfToken(state.csrf_token);
  return state;
}

export async function loadConfig() {
  if (!config) config = await api.authConfig();
  return config;
}

export async function boot() {
  await Promise.all([refresh().catch(() => { state = { authenticated: false }; }), loadConfig()]);
  // The browser knows the viewer's zone and the account may not, or may be stale after travel.
  // Correcting it silently keeps every window and timestamp honest without asking.
  if (state && state.authenticated && state.user) {
    const tz = browserTimezone();
    if (tz && tz !== state.user.tz) {
      try { await api.updateProfile({ tz }); state.user.tz = tz; } catch (e) { /* not important */ }
    }
  }
  return state;
}

export async function logout() {
  try { await api.logout(); } catch (e) { /* already gone */ }
  state = null;
  setCsrfToken("");
  location.hash = "#/login";
  await boot();
}

// ---------------------------------------------------------------------------- shared pieces

function field(labelText, input, hint) {
  return el("label", { class: "field" },
    el("span", { class: "field-label" }, labelText),
    input,
    hint ? el("span", { class: "muted small" }, hint) : null);
}

function errorBox() {
  const box = el("div", { class: "form-error", style: { display: "none" } });
  box.show = (message) => { box.textContent = message; box.style.display = ""; };
  box.hide = () => { box.style.display = "none"; };
  return box;
}

function providerButtons(next = "/") {
  const providers = (config && config.providers) || [];
  if (!providers.length) return null;
  const icons = { google: "G", github: "GH", discord: "D", microsoft: "MS" };
  return el("div", { class: "col gap" },
    el("div", { class: "sso-row" },
      ...providers.map((p) => el("a", {
        class: "btn sso",
        href: `/api/auth/oauth/${p.name}/start?next=${encodeURIComponent(next)}`
              + `&tz=${encodeURIComponent(browserTimezone())}`,
      }, el("span", { class: "sso-badge" }, icons[p.name] || "•"), `Continue with ${p.label}`))),
    el("div", { class: "divider" }, el("span", {}, "or")));
}

/** hCaptcha, loaded only when the server has it configured. */
function captchaWidget() {
  const cfg = config && config.captcha;
  if (!cfg || !cfg.provider) return { node: null, token: () => "" };
  const holder = el("div", { class: "h-captcha", "data-sitekey": cfg.site_key });
  if (!document.getElementById("hcaptcha-script")) {
    document.head.appendChild(el("script", {
      id: "hcaptcha-script", src: "https://js.hcaptcha.com/1/api.js", async: true, defer: true,
    }));
  }
  return {
    node: holder,
    token: () => {
      const input = holder.querySelector('textarea[name="h-captcha-response"]');
      return input ? input.value : "";
    },
  };
}

function shell(title, subtitle, ...content) {
  return el("div", { class: "auth-page" },
    el("div", { class: "auth-card" },
      el("div", { class: "auth-brand" }, "CITAR"),
      el("h1", {}, title),
      subtitle ? el("p", { class: "muted" }, subtitle) : null,
      ...content));
}

function params() {
  return new URLSearchParams(location.hash.split("?")[1] || "");
}

// ---------------------------------------------------------------------------- screens

export async function renderLogin(root) {
  await loadConfig();
  clear(root);
  const q = params();
  const error = errorBox();
  const identifier = el("input", { autocomplete: "username", placeholder: "username or email" });
  const password = el("input", { type: "password", autocomplete: "current-password" });
  const submit = el("button", { class: "primary wide" }, "Sign in");

  if (q.get("verified")) toast("Email confirmed — you can sign in now.", "good", 6000);
  if (q.get("error")) error.show(decodeURIComponent(q.get("error")));

  const go = async () => {
    error.hide();
    submit.disabled = true;
    submit.textContent = "Signing in…";
    try {
      const result = await api.login(identifier.value.trim(), password.value, browserTimezone());
      setCsrfToken(result.csrf_token);
      await refresh();
      location.hash = q.get("next") || "#/";
    } catch (e) {
      error.show(e.message);
      // Offer the way out of the commonest dead end rather than leaving them stuck on it.
      if (/confirm your email/i.test(e.message)) {
        error.append(el("div", { style: { marginTop: "8px" } },
          el("button", {
            class: "link",
            onclick: async () => {
              try {
                await api.resendVerification(identifier.value.trim());
                toast("If that address needs confirming, a new link is on its way.", "good");
              } catch (err) { toast(err.message, "error"); }
            },
          }, "Send the confirmation email again")));
      }
      submit.disabled = false;
      submit.textContent = "Sign in";
    }
  };

  const form = el("form", {
    class: "col gap", onsubmit: (e) => { e.preventDefault(); go(); },
  },
    providerButtons(q.get("next") || "/"),
    config.email_enabled || !(config.providers || []).length
      ? el("div", { class: "col gap" },
          field("Username or email", identifier),
          field("Password", password),
          error,
          submit,
          el("div", { class: "row between small" },
            el("a", { href: "#/forgot" }, "Forgot your password?"),
            config.registration === "open"
              ? el("a", { href: "#/signup" }, "Create an account")
              : el("a", { href: "#/signup" }, "Have an invitation?")))
      : error);

  root.appendChild(shell("Sign in", null,
    config.welcome_message ? el("p", { class: "muted" }, config.welcome_message) : null,
    form,
    config.registration !== "open" && config.closed_message
      ? el("p", { class: "muted small", style: { marginTop: "18px" } }, config.closed_message)
      : null));
  setTimeout(() => identifier.focus(), 40);
}

export async function renderSignup(root) {
  await loadConfig();
  clear(root);
  const q = params();
  const error = errorBox();
  const inviteCode = el("input", { placeholder: "ABCDE-FGHIJ-KLMNO-PQRST", value: q.get("invite") || "" });
  const handle = el("input", { autocomplete: "username", placeholder: "how you appear to others" });
  const email = el("input", { type: "email", autocomplete: "email" });
  const password = el("input", { type: "password", autocomplete: "new-password" });
  const submit = el("button", { class: "primary wide" }, "Create account");
  const inviteNote = el("div", { class: "muted small" });
  const captcha = captchaWidget();

  const checkInvite = async () => {
    const code = inviteCode.value.trim();
    clear(inviteNote);
    if (!code) return;
    try {
      const info = await api.checkInvite(code);
      inviteNote.append(el("span", { class: "good" },
        `Valid invitation${info.invited_by ? ` from ${info.invited_by}` : ""}`
        + (info.role !== "user" ? ` — you will join as ${info.role}.` : ".")));
      if (info.email) email.value = info.email;
    } catch (e) {
      inviteNote.append(el("span", { class: "bad" }, e.message));
    }
  };
  inviteCode.addEventListener("blur", checkInvite);
  if (inviteCode.value) setTimeout(checkInvite, 100);

  const go = async () => {
    error.hide();
    submit.disabled = true;
    submit.textContent = "Creating…";
    try {
      const result = await api.signup({
        handle: handle.value.trim(), email: email.value.trim(), password: password.value,
        invite: inviteCode.value.trim() || null, captcha_token: captcha.token(),
        tz: browserTimezone(),
      });
      clear(root);
      root.appendChild(shell("Check your email",
        result.email_sent === false
          ? "Your account was created, but the confirmation email could not be sent just now."
          : `We sent a confirmation link to ${email.value.trim()}.`,
        el("p", { class: "muted" }, result.message),
        el("a", { class: "btn primary wide", href: "#/login" }, "Back to sign in")));
    } catch (e) {
      error.show(e.message);
      submit.disabled = false;
      submit.textContent = "Create account";
      if (window.hcaptcha) { try { window.hcaptcha.reset(); } catch (err) { /* not rendered */ } }
    }
  };

  const inviteOnly = config.registration !== "open";
  if (config.registration === "closed" && !q.get("invite")) {
    root.appendChild(shell("Registration is closed",
      config.closed_message || "This server is not accepting new accounts.",
      el("a", { class: "btn wide", href: "#/login" }, "Back to sign in")));
    return;
  }

  root.appendChild(shell("Create an account",
    inviteOnly ? "This server is invite-only." : null,
    el("form", { class: "col gap", onsubmit: (e) => { e.preventDefault(); go(); } },
      providerButtons("/"),
      field("Invitation code", inviteCode,
            inviteOnly ? "Required on this server." : "Optional."),
      inviteNote,
      config.email_enabled
        ? el("div", { class: "col gap" },
            field("Username", handle, "Letters, numbers, hyphens and underscores."),
            field("Email", email),
            field("Password", password,
                  `At least ${config.password_min_length} characters. Length matters more than punctuation.`),
            captcha.node,
            error,
            submit)
        : el("p", { class: "muted" },
            "This server has no mail server configured, so accounts with a password cannot be "
            + "created. Use one of the sign-in providers above."),
      el("div", { class: "row center small" }, el("a", { href: "#/login" }, "Already have an account?")))));
}

export async function renderForgot(root) {
  await loadConfig();
  clear(root);
  const email = el("input", { type: "email", autocomplete: "email" });
  const error = errorBox();
  const submit = el("button", { class: "primary wide" }, "Send reset link");

  const go = async () => {
    error.hide();
    submit.disabled = true;
    try {
      const result = await api.forgotPassword(email.value.trim());
      clear(root);
      // Deliberately the same answer whether or not the address is registered.
      root.appendChild(shell("Check your email", result.message,
        el("a", { class: "btn primary wide", href: "#/login" }, "Back to sign in")));
    } catch (e) {
      error.show(e.message);
      submit.disabled = false;
    }
  };

  root.appendChild(shell("Reset your password",
    "We will email you a link to choose a new one.",
    el("form", { class: "col gap", onsubmit: (e) => { e.preventDefault(); go(); } },
      field("Email", email), error, submit,
      el("div", { class: "row center small" }, el("a", { href: "#/login" }, "Back to sign in")))));
}

export async function renderReset(root) {
  await loadConfig();
  clear(root);
  const token = params().get("token") || "";
  const password = el("input", { type: "password", autocomplete: "new-password" });
  const again = el("input", { type: "password", autocomplete: "new-password" });
  const error = errorBox();
  const submit = el("button", { class: "primary wide" }, "Set new password");

  if (!token) {
    root.appendChild(shell("That link is incomplete",
      "Request a new password reset link.",
      el("a", { class: "btn primary wide", href: "#/forgot" }, "Request a link")));
    return;
  }

  const go = async () => {
    error.hide();
    if (password.value !== again.value) { error.show("Those passwords do not match."); return; }
    submit.disabled = true;
    try {
      await api.resetPassword(token, password.value);
      clear(root);
      root.appendChild(shell("Password changed",
        "Every other device has been signed out.",
        el("a", { class: "btn primary wide", href: "#/login" }, "Sign in")));
    } catch (e) {
      error.show(e.message);
      submit.disabled = false;
    }
  };

  root.appendChild(shell("Choose a new password", null,
    el("form", { class: "col gap", onsubmit: (e) => { e.preventDefault(); go(); } },
      field("New password", password,
            `At least ${config.password_min_length} characters.`),
      field("Repeat it", again), error, submit)));
}

/** Shown when a page needs an account and there is not one. */
export function renderSignInWall(root, message) {
  clear(root);
  root.appendChild(shell("Sign in to continue", message || null,
    el("a", {
      class: "btn primary wide",
      href: `#/login?next=${encodeURIComponent(location.hash || "#/")}`,
    }, "Sign in")));
}
