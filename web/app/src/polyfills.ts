import { Buffer as BufferPolyfill } from 'buffer';

type BrowserGlobal = typeof globalThis & {
  Buffer?: typeof BufferPolyfill;
  global?: typeof globalThis;
};

const browserGlobal = globalThis as BrowserGlobal;

browserGlobal.Buffer ??= BufferPolyfill;
browserGlobal.global ??= globalThis;
