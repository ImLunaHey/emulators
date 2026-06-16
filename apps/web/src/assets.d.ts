// Vite asset imports. We don't pull in the full `vite/client` types (see the
// note in main.tsx), so declare just the `?url` suffix we use to bundle binary
// assets (e.g. the open-source PS1 BIOS) through Vite's asset pipeline.
declare module '*?url' {
  const url: string;
  export default url;
}
