// The account page: the account, the profile (who sees what of it), the compute it gave and used, password, linked
// sign-ins, devices, invitations.
import { api } from "./api.js";
import { el, clear, toast, confirmBox } from "./util.js";
import { pageHeader } from "./nav.js";
import * as auth from "./auth.js";
import * as compute from "./compute.js";

function card(title, subtitle, ...content) {
  return el("section", { class: "card" },
    el("h2", {}, title),
    subtitle ? el("p", { class: "muted" }, subtitle) : null,
    ...content);
}

function row(label, value) {
  return el("div", { class: "kv" }, el("span", { class: "k" }, label), el("span", { class: "v" }, value));
}

// Who sees a profile field (design/phase3-features.md P3.2.6): only the account (and administrators), members, or
// everyone; the profile page's own level gates every field on it.
const LEVELS = [["private", "Only me"], ["server", "Members"], ["global", "Everyone"]];
const FIELDS = [["profile", "Profile page", "Nothing below shows to anyone who may not open the page."],
  ["display_name", "Display name", "Hidden, your username shows instead."], ["avatar", "Picture", ""],
  ["bio", "About", ""], ["links", "Links", ""], ["items", "Things you share", "Games and items each still follow their own sharing."],
  ["ratio", "Compute ratio", "What you have given and used of the pooled machines."]];

// The Profile card: what other people see of the account, and who sees each part of it.
function profileCard(me, root) {
  const probation = me.status === "probation";
  const displayName = el("input", { value: me.display_name || "", maxLength: 80, "aria-label": "Display name" });
  const bio = el("textarea", { rows: 4, maxLength: 1000, "aria-label": "About you", style: { width: "100%" } });
  bio.value = me.bio || "";
  const bioCount = el("span", { class: "muted small" });
  const countBio = () => { bioCount.textContent = `${bio.value.length} / 1000`; };
  bio.addEventListener("input", countBio);
  countBio();
  const links = [0, 1, 2].map((i) => {
    const link = (me.links || [])[i] || {};
    return { label: el("input", { value: link.label || "", maxLength: 40, placeholder: "Label", "aria-label": `Link ${i + 1} label`, style: { width: "130px" } }),
      url: el("input", { value: link.url || "", maxLength: 300, placeholder: "https://…", "aria-label": `Link ${i + 1} address`, class: "grow" }) };
  });
  const levels = {};
  const levelRows = FIELDS.map(([field, label, hint]) => {
    const current = (me.field_visibility || {})[field] || "server";
    levels[field] = el("select", { "aria-label": `Who sees: ${label}` }, ...LEVELS.map(([v, text]) =>
      el("option", { value: v, selected: v === current, disabled: v === "global" && probation && current !== "global" }, text)));
    return el("tr", {}, el("td", {}, label, hint ? el("div", { class: "muted small" }, hint) : null), el("td", {}, levels[field]));
  });
  const listed = el("input", { type: "checkbox", checked: me.listed !== false });

  // the picture: the current one, a preview of a chosen file, and its upload
  const preview = el("div", { class: "avatar-preview" });
  const showAvatar = (src) => {
    clear(preview);
    preview.appendChild(src ? el("img", { class: "avatar", src, alt: "Your picture", referrerPolicy: "no-referrer" })
      : el("span", { class: "avatar letter" }, (me.display_name || me.handle)[0].toUpperCase()));
  };
  showAvatar(me.avatar);
  const file = el("input", { type: "file", accept: "image/png,image/jpeg,image/webp", "aria-label": "Choose a picture" });
  const use = el("button", { class: "primary", disabled: true }, "Use this picture");
  let objectUrl = null;
  file.onchange = () => {
    if (objectUrl) URL.revokeObjectURL(objectUrl);
    const f = file.files && file.files[0];
    objectUrl = f ? URL.createObjectURL(f) : null;
    showAvatar(objectUrl || me.avatar);
    use.disabled = !f;
    if (f && f.size > 256 * 1024) toast("That file is over 256 KB; the server will refuse it.", "warn");
  };
  use.onclick = async () => {
    use.disabled = true;
    try {
      await api.uploadAvatar(file.files[0]);
      await auth.refresh();
      toast("Picture saved.", "good");
      renderAccount(clear(root));
    } catch (err) { toast(err.message, "error", 8000); use.disabled = false; }
  };
  const remove = me.avatar_uploaded ? el("button", { class: "danger", onclick: async () => {
    try { await api.deleteAvatar(); await auth.refresh(); toast("Picture removed.", "good"); renderAccount(clear(root)); }
    catch (err) { toast(err.message, "error"); }
  } }, "Remove picture") : null;

  const self = `#/u/${encodeURIComponent(me.handle)}`;
  return card("Profile",
    "What other people see of you. Your username always shows; everything else shows to whom you choose below.",
    el("label", { class: "field" }, el("span", { class: "field-label" }, "Display name"), displayName),
    el("div", { class: "field" }, el("span", { class: "field-label" }, "Picture"),
      el("div", { class: "row gap wrap" }, preview, file, use, remove),
      el("span", { class: "muted small" }, "PNG, JPEG or WebP, at most 256 KB and 1024×1024 pixels."
        + (me.avatar_url && !me.avatar_uploaded ? " Your sign-in provider's picture shows until you upload one." : ""))),
    el("label", { class: "field" }, el("span", { class: "field-label" }, "About you"), bio, bioCount),
    el("div", { class: "field" }, el("span", { class: "field-label" }, "Links (up to three, http or https)"),
      ...links.map((l) => el("div", { class: "row gap" }, l.label, l.url))),
    el("div", { class: "field" }, el("span", { class: "field-label" }, "Who sees what"),
      el("table", { class: "list levels" }, el("tbody", {}, ...levelRows)),
      probation ? el("span", { class: "muted small" }, "New accounts can show their profile to members only.") : null),
    el("label", { class: "row gap" }, listed, el("span", {}, "List me in the members' directory (People)"),
      el("span", { class: "muted small" }, "Unlisted, your page still opens for whoever may see it.")),
    el("div", { class: "row gap wrap" },
      el("button", { class: "primary", onclick: async (e) => {
        e.target.disabled = true;
        try {
          await api.updateProfile({
            display_name: displayName.value, bio: bio.value, listed: listed.checked,
            links: links.filter((l) => l.url.value.trim()).map((l) => ({ label: l.label.value.trim(), url: l.url.value.trim() })),
            field_visibility: Object.fromEntries(Object.entries(levels).map(([k, sel]) => [k, sel.value])),
          });
          await auth.refresh();
          toast("Profile saved.", "good");
        } catch (err) { toast(err.message, "error", 8000); }
        e.target.disabled = false;
      } }, "Save profile"),
      el("a", { href: self }, "See your profile"),
      el("a", { href: `${self}?as=member` }, "View as a member"),
      el("a", { href: `${self}?as=visitor` }, "View as a visitor")));
}

// The Compute card (design/phase3-features.md P3.4.4): what the account's machines gave other members and what its
// work used of theirs and the site's, in credits, with the ratio, by machine and by kind of work, over a range.
function computeCard() {
  const range = el("select", { "aria-label": "Range" }, ...compute.RANGES.map(([v, text]) =>
    el("option", { value: v, selected: v === "30d" }, text)));
  const body = el("div", { class: "col gap" }, el("span", { class: "muted" }, "Loading…"));
  const head = (...cells) => el("thead", {}, el("tr", {}, ...cells.map((c) => el("th", {}, c))));
  // only the latest range's answer is shown: switching twice quickly must not leave the first range's figures
  let asked = 0;
  const show = async () => {
    const mine = ++asked;
    try {
      const d = await api.ledgerMe(range.value);
      if (mine !== asked) return;
      clear(body);
      body.appendChild(el("div", { class: "kv compute-ratio" }, el("span", { class: "k" }, "Ratio (given ÷ used)"),
        el("span", { class: "v" }, el("b", {}, compute.ratioText(d.ratio)))));
      body.appendChild(el("div", { class: "table-wrap" }, el("table", { class: "list compute" },
        head("", "Credits", "GPU time", "CPU time", "Tokens in / out"),
        el("tbody", {},
          el("tr", {}, el("td", {}, "Given"), ...compute.sideCells(d.given).map((c) => el("td", {}, c))),
          el("tr", {}, el("td", {}, "Used"), ...compute.sideCells(d.used).map((c) => el("td", {}, c)))))));
      if (d.by_machine && d.by_machine.length) {
        body.appendChild(el("h3", {}, "By machine"));
        body.appendChild(el("div", { class: "table-wrap" }, el("table", { class: "list compute" },
          head("Machine", "Given", "Used"),
          el("tbody", {}, ...d.by_machine.map((m) => el("tr", {}, el("td", {}, m.name),
            el("td", {}, compute.credits(m.given)), el("td", {}, compute.credits(m.used))))))));
      }
      if (d.by_activity && d.by_activity.length) {
        body.appendChild(el("h3", {}, "By kind of work"));
        body.appendChild(el("div", { class: "table-wrap" }, el("table", { class: "list compute" },
          head("Work", "Given", "Used"),
          el("tbody", {}, ...d.by_activity.map((k) => el("tr", {}, el("td", {}, compute.kindLabel(k.kind)),
            el("td", {}, compute.credits(k.given)), el("td", {}, compute.credits(k.used))))))));
      }
      const own = d.own_use || {};
      body.appendChild(el("span", { class: "muted small" }, own.events
        ? `Your own work on your own machines (counted on neither side): ${compute.credits(own.credits)} credits.`
        : "Your own work on your own machines counts on neither side."));
    } catch (e) {
      if (mine !== asked) return;
      clear(body); body.appendChild(el("span", { class: "bad" }, e.message));
    }
  };
  range.onchange = show;
  show();
  return card("Compute",
    "What your machines gave other members, and what your games, reports, probes and runs used of other members' "
    + "machines and the site's, in credits: a GPU's generation second is about one credit, a CPU core's second a "
    + "twentieth of one, an online API's work its token cost.",
    el("label", { class: "field" }, el("span", { class: "field-label" }, "Range"), range), body);
}

export async function renderAccount(root) {
  const me = auth.user();
  if (!me) { auth.renderSignInWall(root); return {}; }
  const config = auth.authConfig() || {};
  clear(root);
  root.appendChild(pageHeader("account"));
  const page = el("div", { class: "page narrow-page" });
  root.appendChild(page);

  // ------------------------------------------------------------------ account
  const tzInput = el("input", { value: me.tz || "UTC" });
  const sharing = el("select", {},
    el("option", { value: "pool", selected: me.data_sharing === "pool" },
       "Pooled — my results feed the server averages"),
    el("option", { value: "private", selected: me.data_sharing === "private" },
       "Private — my results are mine alone"));

  page.appendChild(card("Account", null,
    row("Username", me.handle),
    row("Email", me.email || "—"),
    row("Role", me.role),
    row("Status", me.status),
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
          await api.updateProfile({ tz: tzInput.value, data_sharing: sharing.value });
          await auth.refresh();
          toast("Saved.", "good");
        } catch (err) { toast(err.message, "error"); }
        e.target.disabled = false;
      },
    }, "Save")));

  // ------------------------------------------------------------------ profile
  page.appendChild(profileCard(me, root));

  // ------------------------------------------------------------------ compute
  page.appendChild(computeCard());

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
