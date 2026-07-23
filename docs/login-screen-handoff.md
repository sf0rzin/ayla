# Login screen handoff

The initial login shell lives in `src/App.tsx` as `LoginPreview`, with its visual treatment grouped near the top of `src/App.css` under the `login-*` classes.

The left `login-art-panel` renders `src/assets/ayla-login-art.png` over a CSS fallback, keeps the inset border treatment in `login-art-panel::after`, and places the product title/description in `login-art-copy`. The right panel switches between sign-in and registration fields in local React state. Registration includes a four-level password strength meter and ends on a “pending administrator activation” state. Submit buttons run an idle/loading/success/error animation while `src/authApi.ts` talks to `https://ayla.rindexx.cc/api/v1`.

Successful login stores the opaque bearer token only in React memory and passes the API user into `WorkspaceApp`; the profile and overview display that user's name, email, and role. Logout revokes the remote session and immediately clears local state. No token is written to local storage. See `docs/auth-api.md` for API operations and administrator activation.
