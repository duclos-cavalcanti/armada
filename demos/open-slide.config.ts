import type { OpenSlideConfig } from '@open-slide/core';

declare const process: { env: Record<string, string | undefined> };

const openSlideConfig: OpenSlideConfig = { slidesDir: `${process.env.DEMO ?? 'fleet'}/slides` };

export default openSlideConfig;
