// The account page: profile, password, linked sign-ins, devices, invitations.
import { api } from "./api.js";
import { el, clear, toast, confirmBox } from "./util.js";
import { pageHeader } from "./nav.js";
import * as auth from "./auth.js";

function card(title, subtitle, ...content) {
  return el("section", { class: "card" },
    el("h2", {}, title),
    subtitle ? el("p", { class: "muted" }, subtitle) : null,
    ...content);
}

function row(label, value) {
  return el("div", { class: "kv" }, el("span", { class: "k" }, label), el("span", { class: "v" }, value));
}

export async function renderAccount(root) {
  const me = auth.user();
  if (!me) { auth.renderSignInWall(root); return {}; }
  const config = auth.authConfig() || {};
  clear(root);
  root.appendChild(pageHeader("account"));
  const page = el("div", { class: "page narrow-page" });
  root.appendChild(page);

  // ------------------------------------------------------------------ profile
  const displayName = el("input", { value: me.display_name || "" });
  const tzInput = el("input", { value: me.tz || "UTC" });
  const sharing = el("select", {},
    el("option", { value: "pool", selected: me.data_sharing === "pool" },
       "Pooled — my results feed the server averages"),
    el("option", { value: "private", selected: me.data_sharing === "private" },
       "Private — my results are mine alone"));

  page.appendChild(card("Profile", null,
    row("Username", me.handle),
    row("Email", me.email || "—"),
    row("Role", me.role),
    row("Status", me.status),
    el("label", { class: "field" }, el("span", { class: "field-label" }, "Display name"), displayName),
    el("label", { class: "field" },
      el("span", { class: "field-label" }, "Time zone"), tzInput,
      el("span", { class: "muted small" },
        "Detected from your browser. Availability windows you set on your servers are stored in "
        + "this zone and shown to other people in theirs.")),
    el("label", { class: "field" },
      el("span", { class: "field-label" }, "Collected data"), sharing,
      el("span", { class: "muted small" },
        "Pooled data is included in server-wide model averages. Private data is excluded from "
        + "every aggregate and only you can analyse it. You can change this at any time and it "
        + "applies retroactively — nothing is baked in at the moment a game is played.")),
    el("button", {
      class: "primary",
      onclick: async (e) => {
        e.target.disabled = true;
        try {
          await api.updateProfile({
            display_name: displayName.value, tz: tzInput.value, data_sharing: sharing.value,
          });
          await auth.refresh();
          toast("Saved.", "good");
        } catch (err) { toast(err.message, "error"); }
        e.target.disabled = false;
      },
    }, "Save profile")));

  // ------------------------------------------------------------------ password
  const current = el("input", { type: "password", autocomplete: "current-password" });
  const fresh = el("input", { type: "password", autocomplete: "new-password" });
  page.appendChild(card(me.has_password ? "Change password" : "Set a password",
    me.has_password
      ? "Changing it signs out every other device."
      : "You sign in with a linked account. Setting a password gives you a second way in.",
    me.has_password
      ? el("label", { class: "field" },
          el("span", { class: "field-label" }, "Current password"), current)
      : null,
    el("label", { class: "field" },
      el("span", { class: "field-label" }, "New password"), fresh,
      el("span", { class: "muted small" },
        `At least ${config.password_min_length || 10} characters.`)),
    el("button", {
      onclick: async (e) => {
        e.target.disabled = true;
        try {
          await api.changePassword(current.value, fresh.value);
          current.value = fresh.value = "";
          toast("Password changed. Other devices were signed out.", "good");
          await auth.refresh();
        } catch (err) { toast(err.message, "error"); }
        e.target.disabled = false;
      },
    }, me.has_password ? "Change password" : "Set password")));

  // ------------------------------------------------------------------ linked accounts
  const providers = (config.providers || []);
  if (providers.length) {
    const linked = new Set(me.providers || []);
    page.appendChild(card("Sign-in methods",
      "Link more than one so losing access to a provider does not lock you out.",
      el("div", { class: "col gap" },
        ...providers.map((p) => el("div", { class: "row between listrow" },
          el("span", {}, p.label),
          linked.has(p.name)
            ? el("button", {
                class: "danger small",
                onclick: async () => {
                  try {
                    await api.unlinkProvider(p.name);
                    toast(`${p.label} unlinked.`, "good");
                    await auth.refresh();
                    renderAccount(root);
                  } catch (err) { toast(err.message, "error"); }
                },
              }, "Unlink")
            : el("a", {
                class: "btn small",
                href: `/api/auth/oauth/${p.name}/start?link=true&next=${encodeURIComponent("#/account")}`,
              }, "Link"))))));
  }

  // ------------------------------------------------------------------ devices
  const devices = el("div", { class: "col gap" }, el("span", { class: "muted" }, "Loading…"));
  page.appendChild(card("Signed-in devices",
    "Anything you do not recognise should be signed out, and your password changed.",
    devices,
    el("button", {
      class: "danger",
      onclick: async () => {
        if (!(await confirmBox("Sign out other devices?",
                               "This session stays signed in; every other one is ended."))) return;
        try {
          const r = await api.revokeOtherSessions();
          toast(`Signed out ${r.revoked} other session(s).`, "good");
          renderAccount(root);
        } catch (err) { toast(err.message, "error"); }
      },
    }, "Sign out other devices")));

  api.sessions().then(({ sessions }) => {
    clear(devices);
    for (const s of sessions) {
      devices.appendChild(el("div", { class: "row between listrow" },
        el("div", { class: "col" },
          el("span", {}, s.current ? el("strong", {}, "This device") : (s.ip || "unknown address")),
          el("span", { class: "muted small" },
            `${(s.user_agent || "").slice(0, 70) || "unknown browser"} · last seen `
            + new Date(s.last_seen_at).toLocaleString())),
        s.current ? null : el("button", {
          class: "small",
          onclick: async () => {
            try { await api.revokeSession(s.id); renderAccount(root); }
            catch (err) { toast(err.message, "error"); }
          },
        }, "Sign out")));
    }
    if (!sessions.length) devices.appendChild(el("span", { class: "muted" }, "None."));
  }).catch((e) => { clear(devices); devices.appendChild(el("span", { class: "bad" }, e.message)); });

  // ------------------------------------------------------------------ invitations
  if (auth.can("invite")) {
    const list = el("div", { class: "col gap" });
    const refreshInvites = async () => {
      clear(list);
      try {
        const { invites, quota } = await api.invites();
        if (!invites.length) {
          list.appendChild(el("span", { class: "muted" }, "No outstanding invitations."));
        }
        for (const inv of invites) {
          list.appendChild(el("div", { class: "row between listrow" },
            el("div", { class: "col" },
              el("code", {}, inv.code),
              el("span", { class: "muted small" },
                `${inv.role} · ${inv.uses}/${inv.max_uses} used`
                + (inv.expires_at ? ` · expires ${new Date(inv.expires_at).toLocaleDateString()}` : "")
                + (inv.email ? ` · for ${inv.email}` : ""))),
            el("div", { class: "row gap" },
              el("button", {
                class: "small",
                onclick: () => {
                  navigator.clipboard.writeText(inv.url);
                  toast("Invitation link copied.", "good");
                },
              }, "Copy link"),
              el("button", {
                class: "danger small",
                onclick: async () => {
                  try { await api.revokeInvite(inv.id); refreshInvites(); }
                  catch (err) { toast(err.message, "error"); }
                },
              }, "Withdraw"))));
        }
        if (quota) list.appendChild(el("span", { class: "muted small" },
          `${quota} outstanding invitation(s) allowed.`));
      } catch (e) {
        list.appendChild(el("span", { class: "bad" }, e.message));
      }
    };

    const roleSelect = el("select", {},
      el("option", { value: "user" }, "user"),
      ...(auth.isAdmin()
        ? [el("option", { value: "moderator" }, "moderator"),
           el("option", { value: "admin" }, "admin")]
        : []));
    const inviteEmail = el("input", { type: "email", placeholder: "optional — pins it to one address" });

    page.appendChild(card("Invitations",
      "Invitation codes let somebody create an account on this server.",
      list,
      el("div", { class: "row gap wrap", style: { marginTop: "12px" } },
        roleSelect, inviteEmail,
        el("button", {
          class: "primary",
          onclick: async (e) => {
            e.target.disabled = true;
            try {
              const inv = await api.createInvite({
                role: roleSelect.value, email: inviteEmail.value.trim() || null,
                send_email: !!inviteEmail.value.trim(),
              });
              inviteEmail.value = "";
              if (inv.warning) toast(inv.warning, "warn", 8000);
              else toast("Invitation created.", "good");
              refreshInvites();
            } catch (err) { toast(err.message, "error"); }
            e.target.disabled = false;
          },
        }, "Create invitation"))));
    refreshInvites();
  }

  // ------------------------------------------------------------------ sign out
  page.appendChild(el("div", { class: "row center", style: { marginTop: "24px" } },
    el("button", { class: "danger", onclick: () => auth.logout() }, "Sign out")));

  return {};
}
