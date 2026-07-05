// @ts-check

const config = {
  title: 'Memzoi',
  tagline: 'Safe project memory for coding agents.',
  favicon: 'img/favicon.ico',

  url: 'https://zokiio.github.io',
  baseUrl: '/memzoi/',

  organizationName: 'Zokiio',
  projectName: 'memzoi',

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
          editUrl: 'https://github.com/Zokiio/memzoi/tree/main/website/',
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
          href: 'https://github.com/Zokiio/memzoi',
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
              href: 'https://github.com/Zokiio/memzoi',
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
