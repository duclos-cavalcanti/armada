import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { viteSingleFile } from 'vite-plugin-singlefile'

// Interactive architecture viewer. The model it renders (graphs/*.json) lives
// inside this project, so no fs.allow escape hatch is needed.
//
// `npm run build` emits a SINGLE self-contained dist/index.html (all JS/CSS/JSON
// inlined) via vite-plugin-singlefile — the artifact published to Splash
// (`npm run publish`), which hosts one file per page at an SSO-gated URL.
export default defineConfig({
  plugins: [react(), viteSingleFile()],
  server: { host: '0.0.0.0' },
})
