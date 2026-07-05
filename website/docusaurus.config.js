// @ts-check

const config = {
  title: 'Memzoi',
  tagline: 'Safe project memory for coding agents.',
  favicon: 'img/favicon.ico',

  url: 'https://zokiio.github.io',
  baseUrl: '/Memzoi/',

  organizationName: 'Zokiio',
  projectName: 'Memzoi',

  onBrokenLinks: 'throw',
  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          routeBasePath: '/',
          sidebarPath: './sidebars.js',
          editUrl: 'https://github.com/Zokiio/Memzoi/tree/main/website/',
          lastVersion: '0.1.0',
          versions: {
            current: {
              label: 'Next',
              path: 'next',
            },
            '0.1.0': {
              label: '0.1.0',
              path: '',
            },
          },
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      },
    ],
  ],

  themeConfig: {
    navbar: {
      title: 'Memzoi',
      items: [
        {to: '/', label: 'Docs', position: 'left'},
        {
          type: 'docsVersionDropdown',
          position: 'right',
        },
        {
          href: 'https://github.com/Zokiio/Memzoi',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [{label: 'Getting Started', to: '/'}],
        },
        {
          title: 'Project',
          items: [
            {
              label: 'GitHub',
              href: 'https://github.com/Zokiio/Memzoi',
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} Memzoi.`,
    },
    prism: {
      additionalLanguages: ['bash', 'json', 'toml'],
    },
  },
};

module.exports = config;
