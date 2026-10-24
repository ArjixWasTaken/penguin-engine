export function rgb2hsv(rgb) {
	const r = rgb.x, g = rgb.y, b = rgb.z;
	const max = Math.max(r, g, b), min = Math.min(r, g, b), d = max - min;
	let h = 0;
	if (d !== 0) {
		if (max === r) h = ((g - b) / d + (g < b ? 6 : 0)) / 6;
		else if (max === g) h = ((b - r) / d + 2) / 6;
		else h = ((r - g) / d + 4) / 6;
	}
	return new Vec3(h, max === 0 ? 0 : d / max, max);
}

export function hsv2rgb(hsv) {
	const h = hsv.x, s = hsv.y, v = hsv.z;
	const i = Math.floor(h * 6), f = h * 6 - i;
	const p = v * (1 - s), q = v * (1 - f * s), t = v * (1 - (1 - f) * s);
	switch (((i % 6) + 6) % 6) {
		case 0: return new Vec3(v, t, p);
		case 1: return new Vec3(q, v, p);
		case 2: return new Vec3(p, v, t);
		case 3: return new Vec3(p, q, v);
		case 4: return new Vec3(t, p, v);
		default: return new Vec3(v, p, q);
	}
}

export function normalizeColor(rgb) {
	return new Vec3(rgb.x / 255, rgb.y / 255, rgb.z / 255);
}

export function expandColor(rgb) {
	return new Vec3(rgb.x * 255, rgb.y * 255, rgb.z * 255);
}
