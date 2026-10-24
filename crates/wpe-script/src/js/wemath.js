export const deg2rad = Math.PI / 180;
export const rad2deg = 180 / Math.PI;

export function smoothStep(min, max, value) {
	const t = Math.max(0, Math.min(1, (value - min) / (max - min)));
	return t * t * (3 - 2 * t);
}

export function mix(a, b, value) {
	return a + (b - a) * value;
}
