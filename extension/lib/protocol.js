// Shared identity constants and provider vocabulary. Pure JS: no shell imports, so it runs
// under `node --test` and inside gnome-shell alike.

export const BUS_NAME = 'org.gnome.Lumen';
export const OBJECT_PATH = '/org/gnome/Lumen';
export const INTERFACE = 'org.gnome.Lumen.Engine';
export const PROTOCOL_MAJOR = 1;
export const SUPPORTED_MAJOR = 1;

export const SCHEMA_ID = 'org.gnome.shell.extensions.lumen';
export const SCHEMA_PATH = '/org/gnome/shell/extensions/lumen/';
export const EXTENSION_UUID = 'lumen@kethan.dev';

export const PROVIDER_IDS = [
    'applications',
    'files',
    'recent',
    'clipboard',
    'calculator',
    'actions',
    'settings',
    'websearch',
];

export const CAPABILITY_IDS = ['files', 'recent', 'clipboard', 'calculator', 'websearch'];

// The overlay's own appearance (schema key `appearance`, contract §8): `system` follows the
// desktop's colour scheme, the other two override it. Shared by the preferences window, which
// offers the choice, and `extension.js`, which applies it to the overlay.
export const APPEARANCE_MODES = ['system', 'light', 'dark'];

// Section order in the result list, from the spec: Top Hit, then applications, files and folders,
// recent documents, clipboard history, calculations and units, system actions, setting toggles,
// and finally the web-search row.
export const SECTION_ORDER = [
    'applications',
    'files',
    'recent',
    'clipboard',
    'calculator',
    'actions',
    'settings',
    'websearch',
];

export const PROVIDER_LABELS = {
    applications: 'Applications',
    files: 'Files and Folders',
    recent: 'Recent Documents',
    clipboard: 'Clipboard',
    calculator: 'Calculation',
    actions: 'Actions',
    settings: 'Settings',
    websearch: 'Web Search',
};

export const SECTION_CAPS = {
    applications: 6,
    files: 8,
    recent: 4,
    clipboard: 5,
    calculator: 1,
    actions: 4,
    settings: 4,
    websearch: 1,
};

export const ACTION_HANDLERS = ['open', 'copy', 'reveal', 'shell'];
export const SHELL_ACTIONS = ['open-url'];

export const ID_PREFIXES = {
    applications: 'app:',
    files: 'file:',
    recent: 'recent:',
    clipboard: 'clip:',
    calculator: 'calc:',
    actions: 'action:',
    settings: 'setting:',
    websearch: 'web:',
};

export function providerOfId(id) {
    for (const provider of PROVIDER_IDS) {
        if (id.startsWith(ID_PREFIXES[provider]))
            return provider;
    }
    return null;
}
