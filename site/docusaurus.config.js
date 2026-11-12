// The notes site (`make serve`, :5175): the repo's markdown, each project's checkout's (the
// product's docs), the fleet viewer (/fleet) and the skills' reports (/reports/).

import fs from 'fs';
import path from 'path';
import { execFileSync } from 'child_process';
import docsPlugin, { validateOptions } from '@docusaurus/plugin-content-docs';
import { Globby, GlobExcludeDefault } from '@docusaurus/utils';

const repo = path.resolve(__dirname, '..');

// The repo's own notes. The code folders (crates/, apps/, hooks/, …) hold none. Globs skip dot
// folders unless named, and the docs link into .claude/ (skills, rules, CLAUDE.md).
const notes = ['*.md', '{agents,skills,local,demos,.claude}/**/*.md'];

// Each project's checkout, as fleetc finds it.
function checkouts() {
  return fs.readdirSync(path.join(repo, 'projects'), { withFileTypes: true })
    .filter((d) => d.isDirectory())
    .flatMap((d) => {
      try {
        const dir = execFileSync('fleetc', ['-C', path.join(repo, 'projects', d.name), 'project'], {
          encoding: 'utf8',
          stdio: ['ignore', 'pipe', 'ignore'],
        }).trim();
        return dir.startsWith(repo + path.sep) ? [path.relative(repo, dir)] : [];
      } catch {
        return [];
      }
    });
}

// Claude Code reads skills' and agents' front matter leniently, so a `description:` may hold an
// unquoted ": ", which YAML rejects. Read those as one `key: value` per line.
function lenientFrontMatter(fileContent) {
  const [, yaml, content] = fileContent.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/) ?? ['', '', fileContent];
  const pairs = yaml.split('\n').map((l) => l.match(/^([\w-]+):\s*(.*)$/)).filter(Boolean);
  return { frontMatter: Object.fromEntries(pairs.map(([, k, v]) => [k, v])), content };
}

// None of this markdown was written for Docusaurus, so keep only its title and description:
// front matter of its own (`tags: ''`, `name:`, `tools:`, …) would be checked against Docusaurus's.
function pageFrontMatter({ title, description, summary }) {
  const page = { title, description: description ?? summary };
  return Object.fromEntries(Object.entries(page).filter(([, v]) => typeof v === 'string'));
}

// Docusaurus watches each include for new files, and every folder under the docs root for
// _category_ files: from the repo root, that walks projects/ (its worktrees and ~70GB of builds).
// So watch only the repo's notes and the sidebar: a page new in a checkout shows on restart (edits
// reload anyway).
async function docs(context, options) {
  const plugin = await docsPlugin(context, options);
  const watched = [...notes.map((p) => path.join(repo, p)), path.resolve(context.siteDir, options.sidebarPath)];
  return { ...plugin, getPathsToWatch: () => watched };
}
docs.validateOptions = validateOptions;

// The fleet viewer (demos/fleet) imports its packages by name: resolve them from site/, so the page
// runs on the site's React, not on a second copy from demos/fleet/node_modules.
function demoPackages(context) {
  const packages = ['react', 'react-dom', '@xyflow/react', 'html-to-image'];
  return {
    name: 'demo-packages',
    configureWebpack: () => ({
      resolve: {
        alias: Object.fromEntries(packages.map((p) => [p, path.join(context.siteDir, 'node_modules', p)])),
      },
    }),
  };
}

// The skills' reports (reports/<project>/…html), listed newest first at /reports. static/reports
// links to the folder, so the pages themselves are served as they are.
function reports() {
  const dir = path.join(repo, 'reports');
  return {
    name: 'reports',
    getPathsToWatch: () => [path.join(dir, '**/*.html')],
    async loadContent() {
      const files = await Globby('**/*.html', { cwd: dir });
      return files
        .map((file) => ({ file, mtime: fs.statSync(path.join(dir, file)).mtimeMs }))
        .sort((a, b) => b.mtime - a.mtime);
    },
    async contentLoaded({ content, actions }) {
      const list = await actions.createData('reports.json', JSON.stringify(content));
      actions.addRoute({ path: '/reports', component: '@site/src/components/Reports.js', modules: { reports: list }, exact: true });
    },
  };
}

export default {
  title: 'Armada',
  url: 'http://localhost',
  baseUrl: '/',
  onBrokenLinks: 'warn',

  markdown: {
    // .md as plain CommonMark, so markdown written for GitHub needn't be valid JSX.
    format: 'detect',
    parseFrontMatter: async (params) => {
      const { frontMatter, content } = await params
        .defaultParseFrontMatter(params)
        .catch(() => lenientFrontMatter(params.fileContent));
      return { frontMatter: pageFrontMatter(frontMatter), content };
    },
  },

  presets: [
    [
      'classic',
      {
        docs: false,
        blog: false,
        theme: { customCss: './src/css/custom.css' },
      },
    ],
  ],

  plugins: [
    [
      docs,
      {
        path: '..',
        include: [...notes, ...checkouts().flatMap((c) => [`${c}/**/*.md`, `${c}/.claude/**/*.md`])],
        // .claude/worktrees/ holds agents' worktrees: whole copies of the checkout.
        exclude: [...GlobExcludeDefault, '**/node_modules/**', '**/target/**', '**/dist/**', '**/.claude/worktrees/**'],
        routeBasePath: '/',
        sidebarPath: './sidebars.js',
      },
    ],
    demoPackages,
    reports,
  ],

  themeConfig: {
    navbar: {
      title: 'Armada',
      items: [
        { to: '/fleet', label: 'Fleet viewer', position: 'left' },
        { to: '/reports', label: 'Reports', position: 'left' },
      ],
    },
    colorMode: { defaultMode: 'dark', disableSwitch: true },
  },
};
