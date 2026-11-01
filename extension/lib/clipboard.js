// Clipboard capture, as far as it can be decided without a shell.
//
// The spec says capture uses `St.Clipboard`'s `owner-changed` signal. **GNOME 50's St has no such
// signal** — `libst-18.so` registers no `StClipboard` signals at all and GNOME's own shell code
// reads the clipboard with the asynchronous `get_text(type, (clipboard, text) => …)`. The shell
// therefore *samples* the clipboard on a timer, and this module holds every decision a sample
// makes, so they can be tested without gnome-shell (see `extension/test/clipboard.test.js`):
//
//   * is there anything to record at all — history on, engine reachable?
//   * is this text new to the sampler, or is it the copy it already recorded?
//   * what is the `ClipboardPush` payload?
//
// If a later GNOME brings the signal back, only `extension.js` changes: `sampler.sample(text)`
// already means "here is what the clipboard holds now".

/** This provider's contract id. */
export const CLIPBOARD_PROVIDER = 'clipboard';

/** The row the daemon sends while history is off (contract §9). */
export const DISABLED_RESULT_ID = 'clip:disabled';

/**
 * How often the shell samples the clipboard, in seconds.
 *
 * A copy is only *read* on the sample, so the recorded timestamp can trail the real copy by up to
 * this much — immaterial for a 30-day recency horizon. Sampling is skipped entirely while history
 * is off, so nothing is read behind the user's back (user story 43).
 */
export const SAMPLE_INTERVAL_SECONDS = 2;

/**
 * Whether a sample should be taken: history is on (the engine's `ClipboardEnabled`) and the engine
 * can take the entry. With the engine gone there is nowhere to record it, so nothing is read.
 */
export function shouldCapture({enabled, engineReady}) {
    return enabled === true && engineReady === true;
}

/**
 * The `ClipboardPush(text, timestamp_ms)` payload for `text`, or `null` when there is nothing to
 * record. The clipboard of a non-text owner (an image, a file) reads back as `''` and is not an
 * entry; the clipboard is text-only by scope.
 */
export function pushPayload(text, timestamp) {
    if (typeof text !== 'string' || text === '')
        return null;

    const ms = Math.trunc(Number(timestamp));
    return {text, timestamp: Number.isFinite(ms) && ms > 0 ? ms : 0};
}

/**
 * The sampler's memory: the text the clipboard held when it was last read, so an unchanged
 * clipboard produces no `ClipboardPush` on every tick.
 *
 * The first sample after a reset counts as new — turning history on records whatever is on the
 * clipboard at that moment, which is what the user asks for by turning it on.
 */
export function createClipboardSampler({now = () => Date.now()} = {}) {
    let last = null;

    return {
        /** The payload for the text just read, or `null` when the clipboard has not changed. */
        sample(text, timestamp = now()) {
            if (text === last)
                return null;

            last = typeof text === 'string' ? text : '';
            return pushPayload(text, timestamp);
        },

        /** Forget what was read; the next sample is treated as new. */
        reset() {
            last = null;
        },

        /** The text last read, for tests and diagnostics. */
        get last() {
            return last;
        },
    };
}
