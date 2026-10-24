import { deg2rad, rad2deg } from 'WEMath';

export function angleVector2(angle) {
	const a = angle * deg2rad;
	return new Vec2(Math.cos(a), Math.sin(a));
}

export function vectorAngle2(direction) {
	return Math.atan2(direction.y, direction.x) * rad2deg;
}
