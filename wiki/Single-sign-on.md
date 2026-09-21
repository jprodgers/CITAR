# Setting up sign-in providers

## First: you are probably on the wrong GitHub form

GitHub has **two** completely different things with confusingly similar names:

| | **OAuth App** ← you want this | **GitHub App** ← not this |
| --- | --- | --- |
| Asks for a **webhook URL** | No | **Yes** |
| Purpose | "Sign in with GitHub" | Bots and integrations that act on repositories |
| Setup | One short form | Permissions, events, installation, private keys |

**If the form is asking you for a webhook, you are creating a GitHub App.** Back out and start
again from the OAuth App page. Nothing in CITAR needs a webhook — it never receives anything from
GitHub, it only sends people there to sign in and gets them back.

---

## GitHub — do this one first

It is the shortest form of the four and it unblocks testing.

1. Go to **https://github.com/settings/developers**
2. Left sidebar: **OAuth Apps** — *not* "GitHub Apps"
3. Click **New OAuth App**

Fill in exactly four fields:

| Field | What to put |
| --- | --- |
| **Application name** | `CITAR` |
| **Homepage URL** | `https://citar.example.com` |
| **Application description** | optional — "Civ Inspired Tool for AI Research" |
| **Authorization callback URL** | `https://citar.example.com/api/auth/oauth/github/callback` |

Leave "Enable Device Flow" unticked.

4. **Register application**
5. You now see a **Client ID**. Click **Generate a new client secret** and copy it — GitHub shows
   it once.

Then on the server:

```bash
sudo citar-secrets
```

Choose **3) GitHub sign-in**, paste the Client ID, paste the secret when prompted (it will not
echo — that is normal), and let it restart CITAR.

Reload the sign-in page and a **Continue with GitHub** button appears.

### The callback URL has to match exactly

Character for character, including `https://` and with **no trailing slash**. GitHub compares it as
a string. A mismatch produces "The redirect_uri MUST match the registered callback URL", which is
GitHub being precise rather than broken.

```
https://citar.example.com/api/auth/oauth/github/callback
```

---

## Google

More ceremony than GitHub, because Google requires a consent screen.

1. **https://console.cloud.google.com** → create a project (call it CITAR)
2. **APIs & Services → OAuth consent screen**
   - User type: **External**
   - App name `CITAR`, your email as support contact and developer contact
   - Scopes: add `openid`, `.../auth/userinfo.email`, `.../auth/userinfo.profile` — nothing else
   - **Test users**: add your own address and your friends'. While the app is in "Testing" only
     those addresses can sign in, which for a handful of people is *the point* — you never have to
     go through Google's verification review.
3. **APIs & Services → Credentials → Create Credentials → OAuth client ID**
   - Application type: **Web application**
   - **Authorised redirect URIs** → ADD URI:
     `https://citar.example.com/api/auth/oauth/google/callback`
   - (Leave "Authorised JavaScript origins" empty — CITAR's flow is server-side.)
4. Copy the Client ID and Client secret → `sudo citar-secrets` → **4) Google sign-in**

---

## Discord

1. **https://discord.com/developers/applications** → **New Application** → name it CITAR
2. **OAuth2** in the sidebar
3. **Redirects → Add Redirect**:
   `https://citar.example.com/api/auth/oauth/discord/callback`
4. Save
5. Copy the **Client ID**, then **Reset Secret** to reveal a client secret
6. `sudo citar-secrets` → **5) Discord sign-in**

Scopes are requested by CITAR at sign-in time (`identify`, `email`) — you do not preselect them.

---

## Microsoft

1. **https://entra.microsoft.com** → **App registrations** → **New registration**
2. Name: `CITAR`
3. Supported account types: **Accounts in any organizational directory and personal Microsoft
   accounts** (this is what `tenant=common` means)
4. **Redirect URI**: platform **Web**, value
   `https://citar.example.com/api/auth/oauth/microsoft/callback`
5. Register. Copy the **Application (client) ID** from the overview page.
6. **Certificates & secrets → New client secret** → copy the **Value** (not the Secret ID — the
   Value is the one you need, and it is only shown now)
7. `sudo citar-secrets` → **6) Microsoft sign-in**

---

## After adding any provider

```bash
sudo citar-secrets show      # confirms the ID is set and the secret is present
```

Then open https://citar.example.com in a private window. The buttons appear automatically —
a provider with no credentials configured simply does not show up, so there is nothing to enable.

### If sign-in fails

| What you see | What it means |
| --- | --- |
| "redirect_uri MUST match" | The callback URL differs from what you registered. Compare character by character. |
| "That sign-in attempt could not be verified" | The ten-minute state cookie expired, or cookies are blocked. Start again from the sign-in page. |
| "already uses that address, and X did not confirm it" | The provider did not vouch for the email. Sign in with your password and link the account from the account page instead — CITAR will not auto-link an unverified address, because that is account takeover. |
| The button does not appear | The client ID or secret is missing. `sudo citar-secrets show`. |

Logs: `sudo journalctl -u citar -f` while you try.


---

*This page is generated from [`docs/server/OAUTH.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/OAUTH.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
