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
				{
					label: 'Core',
					items: [
						{ label: 'Overview', slug: 'layers/core/overview' },
						{ label: 'Identity & IDs', slug: 'layers/core/id' },
						{ label: 'Error Handling', slug: 'layers/core/error' },
						{ label: 'Crypto', slug: 'layers/core/crypto' },
						{ label: 'IPv6 Addressing', slug: 'layers/core/addressing' },
						{ label: 'Validation', slug: 'layers/core/validate' },
						{ label: 'Configuration', slug: 'layers/core/config' },
						{ label: 'Versioning', slug: 'layers/core/version' },
						{ label: 'Process', slug: 'layers/core/process' },
						{ label: 'Transport', slug: 'layers/core/transport' },
						{ label: 'Logging', slug: 'layers/core/logging' },
						{
							label: 'Resource Framework',
							items: [
								{ label: 'Overview', slug: 'layers/core/resource/overview' },
								{ label: 'Identity', slug: 'layers/core/resource/identity' },
								{ label: 'Scope', slug: 'layers/core/resource/scope' },
								{ label: 'Schema', slug: 'layers/core/resource/schema' },
								{ label: 'Operations', slug: 'layers/core/resource/operation' },
								{ label: 'Constraints', slug: 'layers/core/resource/constraint' },
								{ label: 'Presentation', slug: 'layers/core/resource/presentation' },
								{ label: 'Builder', slug: 'layers/core/resource/builder' },
								{ label: 'CLI Generation', slug: 'layers/core/resource/cli-gen' },
								{ label: 'Dispatch', slug: 'layers/core/resource/dispatch' },
								{ label: 'Registry', slug: 'layers/core/resource/registry' },
							],
						},
						{
							label: 'UI',
							items: [
								{ label: 'Tables', slug: 'layers/core/ui/table' },
								{ label: 'Colors', slug: 'layers/core/ui/color' },
								{ label: 'Prompts', slug: 'layers/core/ui/prompt' },
								{ label: 'Confirmations', slug: 'layers/core/ui/confirm' },
								{ label: 'Spinner', slug: 'layers/core/ui/spinner' },
								{ label: 'Time Formatting', slug: 'layers/core/ui/time-fmt' },
							],
						},
						{
							label: 'API',
							items: [
								{ label: 'Route Generation', slug: 'layers/core/api/route-gen' },
								{ label: 'Server', slug: 'layers/core/api/server' },
								{ label: 'Middleware', slug: 'layers/core/api/middleware' },
								{ label: 'Error Responses', slug: 'layers/core/api/error-response' },
							],
						},
					],
				},
				{
					label: 'State',
					items: [
						{ label: 'Overview', slug: 'layers/state/overview' },
					],
				},
				{
					label: 'Hypervisor',
					items: [
						{ label: 'Overview', slug: 'layers/hypervisor/overview' },
						{ label: 'Handlers', slug: 'layers/hypervisor/handlers' },
						{
							label: 'Fabric',
							items: [
								{ label: 'Mesh', slug: 'layers/hypervisor/fabric/mesh' },
								{ label: 'Peers', slug: 'layers/hypervisor/fabric/peer' },
								{ label: 'State', slug: 'layers/hypervisor/fabric/state' },
								{ label: 'Operations', slug: 'layers/hypervisor/fabric/ops' },
								{ label: 'WireGuard', slug: 'layers/hypervisor/fabric/wg' },
								{ label: 'Service', slug: 'layers/hypervisor/fabric/service' },
								{ label: 'Peering Protocol', slug: 'layers/hypervisor/fabric/peering' },
								{ label: 'Peering Server', slug: 'layers/hypervisor/fabric/peering-server' },
								{ label: 'Peering Client', slug: 'layers/hypervisor/fabric/peering-client' },
							],
						},
					],
				},
			],
		}),
	],
});
