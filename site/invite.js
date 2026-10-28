// The invite landing page's logic (D25): an `https://<host>/j/#1/<room>/<secret>[/<name>]` link
// opens `mic://join/1/<room>#<secret>[/<name>]`, the room's name as it was, and anything after it,
// a later field, too. The invite is in the fragment, which a browser never sends to a
// server, and it goes nowhere else: no request is made with it, and it's taken out of the address
// bar once read. The forms are mic-core's (crates/mic-core/src/invite.rs); a test there and
// invite.test.mjs here hold them to the same invite.

/** The invite format this page reads. */
export const VERSION = "1";

const BASE64URL = /^[A-Za-z0-9_-]+$/;

/**
 * The `mic://` link a landing page's fragment (with or without its `#`) stands for, or why it
 * doesn't stand for one.
 * @param {string} fragment
 * @returns {{ link: string } | { error: string }}
 */
export function micLink(fragment) {
    const parts = fragment.replace(/^#/, "").split("/");
    if (parts.length < 3) {
        return { error: "This link holds no invite." };
    }
    const [version, room, secret, ...after] = parts;
    if (version !== VERSION) {
        return { error: `This invite is of format ${version}; this page reads format ${VERSION}.` };
    }
    // 16 bytes and 32, in base64url without padding.
    if (room.length !== 22 || !BASE64URL.test(room)) {
        return { error: "This invite's room isn't one." };
    }
    if (secret.length !== 43 || !BASE64URL.test(secret)) {
        return { error: "This invite's secret isn't one." };
    }
    // The name, and what comes after it, base64url each, as mic reads them.
    if (after.some((part) => part !== "" && !BASE64URL.test(part))) {
        return { error: "This invite's name isn't one." };
    }
    const rest = after.length > 0 ? `/${after.join("/")}` : "";
    return { link: `mic://join/${version}/${room}#${secret}${rest}` };
}

/** Fills the landing page: the link to open, or why there's none; then forgets the fragment. */
export function land(document, location, history) {
    const found = micLink(location.hash);
    const open = document.getElementById("open");
    const message = document.getElementById("message");
    if ("error" in found) {
        message.textContent = found.error;
        open.hidden = true;
        return;
    }
    open.href = found.link;
    message.textContent = "Opening the room in mic. If nothing happens, mic isn't installed yet.";
    // Out of the address bar, its history and whatever a screenshot of it shows.
    history.replaceState(null, "", location.pathname);
    location.href = found.link;
}

if (typeof document !== "undefined") {
    land(document, window.location, window.history);
}
