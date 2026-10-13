'use strict';
// Shared by the desktop launch page and the native-only Telegram window.
// Unknown machine errors stay behind a safe, user-facing fallback.
(() => {
  const messages = Object.freeze({
    app_credentials_missing: 'This Dendrite build needs Telegram application credentials. Ask the developer to configure its API ID and API hash.',
    import_already_running: 'A Telegram snapshot is already open. Continue in its Dendrite window.',
    import_already_complete: 'This snapshot is complete. Start a fresh snapshot to import more history.',
    import_unavailable: 'This snapshot is unavailable. Start a fresh snapshot from Dendrite’s Telegram tab.',
    consent_unavailable: 'Your permission could not be saved or checked. Reconnect your Synthius account if needed, then retry. Telegram import has not started.',
    consent_in_progress: 'Your permission is being saved. Please wait.',
    consent_required: 'Read the Telegram import explanation and choose I agree and continue first.',
    worker_unavailable: 'The Telegram importer needs a fresh start. Restart Dendrite, then try again.',
    resume_identity_mismatch: 'Sign in to the Telegram account used for this snapshot. The saved chat and date selection is fixed.',
    logout_unconfirmed: 'Check Telegram Settings → Devices and terminate this Dendrite session to confirm sign-out. Its local authorization was discarded.',
    auto_delete_chat_excluded: 'This chat uses auto-delete. Choose a chat with ordinary retained history.',
    invalid_code: 'The login code is invalid or expired. Request a fresh code.',
    invalid_password: 'Check your Telegram two-step verification password.',
    telegram_network_blocked: 'Telegram network access is blocked for this Dendrite process. Close Dendrite, open it normally from your applications menu, then try again.',
    telegram_unavailable: 'Dendrite could not reach Telegram. Check your connection and select Show QR code again.',
    telegram_timeout: 'Telegram did not respond in time. Check your connection and select Show QR code again.',
    qr_expired: 'The Telegram QR code expired. Select Show QR code to request a fresh one.',
    session_expired: 'The temporary Telegram login session expired. Start a fresh snapshot from Dendrite’s Telegram tab.',
    telegram_rate_limit: 'Telegram asked this import to wait. Your saved pages remain available; resume later.',
    telegram_requires_official_app: 'Complete this Telegram account step in the official app, then start a fresh import.',
    history_cursor_stalled: 'Telegram returned a repeated history page. The saved pages are preserved for a fresh retry.',
    intent_expired: 'Return to Sources and open a fresh import.',
    invalid_selection: 'Select between 1 and 20 chats and a valid date range.',
  });
  globalThis.SynthiusTelegramErrorMessage = (code, fallback) =>
    typeof code === 'string' && Object.hasOwn(messages, code) ? messages[code] : fallback;
})();
