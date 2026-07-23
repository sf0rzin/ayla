# Login screen handoff

The initial login shell lives in `src/App.tsx` as `LoginPreview`, with its visual treatment grouped near the top of `src/App.css` under the `login-*` classes.

Current scope is intentionally visual only. The left `login-art-panel` renders `src/assets/ayla-login-art.png` over a CSS fallback, keeps the inset border treatment in `login-art-panel::after`, and places the product title/description in `login-art-copy`. The right panel switches between sign-in and registration fields in local React state. Registration includes a four-level password strength meter and ends on a local “pending administrator activation” state. Submit buttons run an idle/loading/success/error animation before either opening `WorkspaceApp` or showing the pending state. These outcomes currently use local validation only: fields are not submitted, and no authentication state, credential persistence, or backend commands have been added.

For the next pass, replace the preview action with the real login flow and decide where authenticated state should live before adding persistence. Keep `WorkspaceApp` isolated so the existing dashboard behavior remains unchanged.
