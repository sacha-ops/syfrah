// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
	site: 'https://sacha-ops.github.io',
	base: '/syfrah',
	integrations: [
		starlight({
			title: 'Syfrah',
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/sacha-ops/syfrah' }],
			sidebar: [
				{
					label: 'Getting Started',
					items: [
						{ label: 'Introduction', slug: '' },
					],
				},
				{ label: 'Core', autogenerate: { directory: 'layers/core' } },
				{ label: 'State', autogenerate: { directory: 'layers/state' } },
				{ label: 'Hypervisor', autogenerate: { directory: 'layers/hypervisor' } },
				{
					label: 'API Reference',
					items: [
						{ label: 'REST API (Scalar)', link: '/syfrah/rest/' },
						{ label: 'Rust API (rustdoc)', link: '/syfrah/api/syfrah_core/' },
					],
				},
			],
		}),
	],
});
