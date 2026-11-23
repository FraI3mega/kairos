//   gjs gnome-shell-harness.js modern|legacy PID,PID,...
const {Gio, GLib} = imports.gi;

const [variant, pidList] = imports.system.programArgs;
const pids = pidList.split(',').map(Number);
const dir = `${GLib.path_get_dirname(imports.system.programInvocationName)}/../../packaging/gnome-extension/${variant}`;

let source = new TextDecoder().decode(Gio.File.new_for_path(`${dir}/extension.js`).load_contents(null)[1]);
if (variant === 'modern') {
    source = source
        .replace(/^import Gio from 'gi:\/\/Gio';$/m, 'const Gio = imports.gi.Gio;')
        .replace(/^import \{Extension\} from .*$/m, 'class Extension {}')
        .replace('export default class KairosFocus', 'var KairosFocus = class KairosFocus');
}
const make = new Function('global', `${source}\nreturn ${variant === 'modern' ? 'new KairosFocus()' : 'init()'};`);

const handlers = new Map();
let nextId = 1;
const display = {
    focus_window: null,
    connect: (_signal, callback) => {
        handlers.set(nextId, callback);
        return nextId++;
    },
    disconnect: id => handlers.delete(id),
};
const windowFor = pid => ({
    get_pid: () => pid,
    get_wm_class: () => `class-${pid}`,
    get_title: () => `title-${pid}`,
    connect: () => 1,
    disconnect: () => {},
});

const extension = make({display});
const loop = GLib.MainLoop.new(null, false);

Gio.bus_own_name(Gio.BusType.SESSION, 'org.gnome.Shell', Gio.BusNameOwnerFlags.NONE, null,
    () => {
        extension.enable();
        let i = 0;
        GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            display.focus_window = windowFor(pids[i++ % pids.length]);
            handlers.forEach(callback => callback());
            return GLib.SOURCE_CONTINUE;
        });
    },
    () => loop.quit());
loop.run();
