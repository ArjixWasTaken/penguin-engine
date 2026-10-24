// Vector, matrix and script-property classes every SceneScript sees as globals.
// Used when Wallpaper Engine's own assets/scripts/jsclasses/baseclasses.js is
// not supplied; mirrors the API declared in lib.sceneScript.d.ts.
const _Epsilon = 0.00001;
const deg2rad = Math.PI / 180;
const rad2deg = 180 / Math.PI;

function _splitFloats(s) {
	return String(s).trim().split(/\s+/).map(parseFloat);
}
function _comp(f, k) {
	return typeof f === 'number' ? f : f[k];
}

class Vec2 {
	constructor(x, y) {
		if (typeof x === 'string') {
			const p = _splitFloats(x);
			this.x = p[0]; this.y = p.length > 1 ? p[1] : p[0];
		} else if (x !== null && typeof x === 'object') {
			this.x = x.x; this.y = x.y;
		} else if (typeof x !== 'undefined') {
			this.x = x; this.y = typeof y === 'number' ? y : x;
		} else {
			this.x = 0; this.y = 0;
		}
	}
	length() { return Math.sqrt(this.x * this.x + this.y * this.y); }
	lengthSqr() { return this.x * this.x + this.y * this.y; }
	distance(v) { return this.subtract(v).length(); }
	distanceSqr(v) { return this.subtract(v).lengthSqr(); }
	normalize() { return this.divide(this.length()); }
	copy() { return new Vec2(this.x, this.y); }
	equals(f) {
		return f instanceof Vec2 && Math.abs(this.x - f.x) < _Epsilon && Math.abs(this.y - f.y) < _Epsilon;
	}
	isFinite() { return Number.isFinite(this.x) && Number.isFinite(this.y); }
	negate() { return new Vec2(-this.x, -this.y); }
	add(f) { return new Vec2(this.x + _comp(f, 'x'), this.y + _comp(f, 'y')); }
	subtract(f) { return new Vec2(this.x - _comp(f, 'x'), this.y - _comp(f, 'y')); }
	multiply(f) { return new Vec2(this.x * _comp(f, 'x'), this.y * _comp(f, 'y')); }
	divide(f) { return new Vec2(this.x / _comp(f, 'x'), this.y / _comp(f, 'y')); }
	dot(f) { return this.x * f.x + this.y * f.y; }
	reflect(n) { return this.subtract(n.multiply(2 * this.dot(n))); }
	perpendicular() { return new Vec2(this.y, -this.x); }
	project(v) { return v.multiply(this.dot(v) / v.lengthSqr()); }
	angle() { return Math.atan2(this.y, this.x) * rad2deg; }
	angleBetween(v) {
		const d = this.length() * v.length();
		return d === 0 ? 0 : Math.acos(Math.max(-1, Math.min(1, this.dot(v) / d))) * rad2deg;
	}
	rotate(angle) {
		const a = angle * deg2rad, c = Math.cos(a), s = Math.sin(a);
		return new Vec2(this.x * c - this.y * s, this.x * s + this.y * c);
	}
	mix(v, a) {
		return new Vec2(this.x + (v.x - this.x) * _comp(a, 'x'), this.y + (v.y - this.y) * _comp(a, 'y'));
	}
	min(v) { return new Vec2(Math.min(this.x, _comp(v, 'x')), Math.min(this.y, _comp(v, 'y'))); }
	max(v) { return new Vec2(Math.max(this.x, _comp(v, 'x')), Math.max(this.y, _comp(v, 'y'))); }
	clamp(lo, hi) { return this.max(lo).min(hi); }
	abs() { return new Vec2(Math.abs(this.x), Math.abs(this.y)); }
	sign() { return new Vec2(Math.sign(this.x), Math.sign(this.y)); }
	round() { return new Vec2(Math.round(this.x), Math.round(this.y)); }
	floor() { return new Vec2(Math.floor(this.x), Math.floor(this.y)); }
	ceil() { return new Vec2(Math.ceil(this.x), Math.ceil(this.y)); }
	fract() { return this.subtract(this.floor()); }
	mod(v) {
		const mx = _comp(v, 'x'), my = _comp(v, 'y');
		return new Vec2(this.x - mx * Math.floor(this.x / mx), this.y - my * Math.floor(this.y / my));
	}
	step(e) { return new Vec2(this.x < _comp(e, 'x') ? 0 : 1, this.y < _comp(e, 'y') ? 0 : 1); }
	smoothStep(lo, hi) {
		const f = (v, a, b) => { const t = Math.max(0, Math.min(1, (v - a) / (b - a))); return t * t * (3 - 2 * t); };
		return new Vec2(f(this.x, _comp(lo, 'x'), _comp(hi, 'x')), f(this.y, _comp(lo, 'y'), _comp(hi, 'y')));
	}
	toString() { return this.x + ' ' + this.y; }
	toConfigString() { return this.toString(); }
}

class Vec3 {
	constructor(x, y, z) {
		if (typeof x === 'string') {
			const p = _splitFloats(x);
			if (p.length === 1) { this.x = this.y = this.z = p[0]; }
			else { this.x = p[0]; this.y = p[1]; this.z = p.length > 2 ? p[2] : 0; }
		} else if (x !== null && typeof x === 'object') {
			this.x = x.x; this.y = x.y; this.z = typeof x.z === 'number' ? x.z : 0;
		} else if (typeof x !== 'undefined') {
			this.x = x;
			this.y = typeof y === 'number' ? y : x;
			this.z = typeof z === 'number' ? z : (typeof y === 'number' ? 0 : x);
		} else {
			this.x = 0; this.y = 0; this.z = 0;
		}
	}
	static fromSpherical(r, theta, phi) {
		const t = theta * deg2rad, p = phi * deg2rad, st = Math.sin(t);
		return new Vec3(r * st * Math.cos(p), r * Math.cos(t), r * st * Math.sin(p));
	}
	get r() { return this.x; } set r(v) { this.x = v; }
	get g() { return this.y; } set g(v) { this.y = v; }
	get b() { return this.z; } set b(v) { this.z = v; }
	length() { return Math.sqrt(this.lengthSqr()); }
	lengthSqr() { return this.x * this.x + this.y * this.y + this.z * this.z; }
	distance(v) { return this.subtract(v).length(); }
	distanceSqr(v) { return this.subtract(v).lengthSqr(); }
	normalize() { return this.divide(this.length()); }
	copy() { return new Vec3(this.x, this.y, this.z); }
	equals(f) {
		return f instanceof Vec3 && Math.abs(this.x - f.x) < _Epsilon &&
			Math.abs(this.y - f.y) < _Epsilon && Math.abs(this.z - f.z) < _Epsilon;
	}
	isFinite() { return Number.isFinite(this.x) && Number.isFinite(this.y) && Number.isFinite(this.z); }
	negate() { return new Vec3(-this.x, -this.y, -this.z); }
	_op(f, op, keepZ) {
		if (typeof f === 'number') return new Vec3(op(this.x, f), op(this.y, f), op(this.z, f));
		const hasZ = typeof f.z === 'number';
		return new Vec3(op(this.x, f.x), op(this.y, f.y), hasZ ? op(this.z, f.z) : keepZ(this.z));
	}
	add(f) { return this._op(f, (a, b) => a + b, z => z); }
	subtract(f) { return this._op(f, (a, b) => a - b, z => z); }
	multiply(f) { return this._op(f, (a, b) => a * b, z => z); }
	divide(f) { return this._op(f, (a, b) => a / b, z => z); }
	cross(f) {
		return new Vec3(this.y * f.z - this.z * f.y, this.z * f.x - this.x * f.z, this.x * f.y - this.y * f.x);
	}
	dot(f) { return this.x * f.x + this.y * f.y + this.z * f.z; }
	reflect(n) { return this.subtract(n.multiply(2 * this.dot(n))); }
	refract(n, eta) {
		const d = this.dot(n), k = 1 - eta * eta * (1 - d * d);
		if (k < 0) return new Vec3(0, 0, 0);
		return this.multiply(eta).subtract(n.multiply(eta * d + Math.sqrt(k)));
	}
	project(v) { return v.multiply(this.dot(v) / v.lengthSqr()); }
	angleBetween(v) {
		const d = this.length() * v.length();
		return d === 0 ? 0 : Math.acos(Math.max(-1, Math.min(1, this.dot(v) / d))) * rad2deg;
	}
	toSpherical() {
		const r = this.length();
		if (r === 0) return new Vec3(0, 0, 0);
		return new Vec3(r, Math.acos(this.y / r) * rad2deg, Math.atan2(this.z, this.x) * rad2deg);
	}
	mix(v, a) {
		return new Vec3(this.x + (v.x - this.x) * _comp(a, 'x'), this.y + (v.y - this.y) * _comp(a, 'y'),
			this.z + (v.z - this.z) * _comp(a, 'z'));
	}
	min(v) { return new Vec3(Math.min(this.x, _comp(v, 'x')), Math.min(this.y, _comp(v, 'y')), Math.min(this.z, _comp(v, 'z'))); }
	max(v) { return new Vec3(Math.max(this.x, _comp(v, 'x')), Math.max(this.y, _comp(v, 'y')), Math.max(this.z, _comp(v, 'z'))); }
	clamp(lo, hi) { return this.max(lo).min(hi); }
	abs() { return new Vec3(Math.abs(this.x), Math.abs(this.y), Math.abs(this.z)); }
	sign() { return new Vec3(Math.sign(this.x), Math.sign(this.y), Math.sign(this.z)); }
	round() { return new Vec3(Math.round(this.x), Math.round(this.y), Math.round(this.z)); }
	floor() { return new Vec3(Math.floor(this.x), Math.floor(this.y), Math.floor(this.z)); }
	ceil() { return new Vec3(Math.ceil(this.x), Math.ceil(this.y), Math.ceil(this.z)); }
	fract() { return this.subtract(this.floor()); }
	mod(v) {
		const m = (a, b) => a - b * Math.floor(a / b);
		return new Vec3(m(this.x, _comp(v, 'x')), m(this.y, _comp(v, 'y')), m(this.z, _comp(v, 'z')));
	}
	step(e) {
		return new Vec3(this.x < _comp(e, 'x') ? 0 : 1, this.y < _comp(e, 'y') ? 0 : 1, this.z < _comp(e, 'z') ? 0 : 1);
	}
	smoothStep(lo, hi) {
		const f = (v, a, b) => { const t = Math.max(0, Math.min(1, (v - a) / (b - a))); return t * t * (3 - 2 * t); };
		return new Vec3(f(this.x, _comp(lo, 'x'), _comp(hi, 'x')), f(this.y, _comp(lo, 'y'), _comp(hi, 'y')),
			f(this.z, _comp(lo, 'z'), _comp(hi, 'z')));
	}
	toString() { return this.x + ' ' + this.y + ' ' + this.z; }
	toConfigString() { return this.toString(); }
}

class Vec4 {
	constructor(x, y, z, w) {
		if (typeof x === 'string') {
			const p = _splitFloats(x);
			if (p.length === 1) { this.x = this.y = this.z = this.w = p[0]; }
			else { this.x = p[0]; this.y = p[1]; this.z = p[2] || 0; this.w = p[3] || 0; }
		} else if (x !== null && typeof x === 'object') {
			this.x = x.x; this.y = x.y;
			this.z = typeof x.z === 'number' ? x.z : 0;
			this.w = typeof x.w === 'number' ? x.w : 0;
		} else if (typeof x !== 'undefined') {
			if (typeof y === 'number') {
				this.x = x; this.y = y; this.z = z || 0; this.w = w || 0;
			} else {
				this.x = this.y = this.z = this.w = x;
			}
		} else {
			this.x = 0; this.y = 0; this.z = 0; this.w = 0;
		}
	}
	get r() { return this.x; } set r(v) { this.x = v; }
	get g() { return this.y; } set g(v) { this.y = v; }
	get b() { return this.z; } set b(v) { this.z = v; }
	get a() { return this.w; } set a(v) { this.w = v; }
	_map(f, op) {
		return new Vec4(op(this.x, _comp(f, 'x')), op(this.y, _comp(f, 'y')), op(this.z, _comp(f, 'z')), op(this.w, _comp(f, 'w')));
	}
	length() { return Math.sqrt(this.lengthSqr()); }
	lengthSqr() { return this.dot(this); }
	distance(v) { return this.subtract(v).length(); }
	distanceSqr(v) { return this.subtract(v).lengthSqr(); }
	normalize() { return this.divide(this.length()); }
	copy() { return new Vec4(this.x, this.y, this.z, this.w); }
	equals(f) {
		return f instanceof Vec4 && Math.abs(this.x - f.x) < _Epsilon && Math.abs(this.y - f.y) < _Epsilon &&
			Math.abs(this.z - f.z) < _Epsilon && Math.abs(this.w - f.w) < _Epsilon;
	}
	isFinite() { return [this.x, this.y, this.z, this.w].every(Number.isFinite); }
	negate() { return new Vec4(-this.x, -this.y, -this.z, -this.w); }
	add(f) { return this._map(f, (a, b) => a + b); }
	subtract(f) { return this._map(f, (a, b) => a - b); }
	multiply(f) { return this._map(f, (a, b) => a * b); }
	divide(f) { return this._map(f, (a, b) => a / b); }
	dot(f) { return this.x * f.x + this.y * f.y + this.z * f.z + this.w * f.w; }
	reflect(n) { return this.subtract(n.multiply(2 * this.dot(n))); }
	project(v) { return v.multiply(this.dot(v) / v.lengthSqr()); }
	mix(v, a) { return this.add(new Vec4(v).subtract(this).multiply(a)); }
	min(v) { return this._map(v, Math.min); }
	max(v) { return this._map(v, Math.max); }
	clamp(lo, hi) { return this.max(lo).min(hi); }
	abs() { return this._map(0, (a) => Math.abs(a)); }
	sign() { return this._map(0, (a) => Math.sign(a)); }
	round() { return this._map(0, (a) => Math.round(a)); }
	floor() { return this._map(0, (a) => Math.floor(a)); }
	ceil() { return this._map(0, (a) => Math.ceil(a)); }
	fract() { return this.subtract(this.floor()); }
	mod(v) { return this._map(v, (a, b) => a - b * Math.floor(a / b)); }
	step(e) { return this._map(e, (a, b) => (a < b ? 0 : 1)); }
	smoothStep(lo, hi) {
		const t = this.subtract(lo).divide(new Vec4(hi).subtract(lo)).max(0).min(1);
		return t.multiply(t).multiply(t.multiply(-2).add(3));
	}
	toString() { return this.x + ' ' + this.y + ' ' + this.z + ' ' + this.w; }
	toConfigString() { return this.toString(); }
}

class Mat3 {
	constructor(arg) {
		if (arg instanceof Mat3) this.m = arg.m.slice();
		else if (Array.isArray(arg) && arg.length === 9) this.m = arg.slice();
		else if (typeof arg === 'string' && _splitFloats(arg).length === 9) this.m = _splitFloats(arg);
		else this.m = [1, 0, 0, 0, 1, 0, 0, 0, 1];
	}
	static identity() { return new Mat3(); }
	static fromTranslation(v) { const r = new Mat3(); r.m[6] = v.x; r.m[7] = v.y; return r; }
	static fromScale(v) { const r = new Mat3(); r.m[0] = _comp(v, 'x'); r.m[4] = _comp(v, 'y'); return r; }
	static fromRotation(angle) {
		const a = angle * deg2rad, c = Math.cos(a), s = Math.sin(a), r = new Mat3();
		r.m[0] = c; r.m[1] = s; r.m[3] = -s; r.m[4] = c;
		return r;
	}
	static fromBasis(right, up) { return new Mat3([right.x, right.y, 0, up.x, up.y, 0, 0, 0, 1]); }
	static fromMat4(m) {
		const a = m.m;
		return new Mat3([a[0], a[1], 0, a[4], a[5], 0, a[12], a[13], 1]);
	}
	static compose(t, rot, s) {
		return Mat3.fromTranslation(t).multiply(Mat3.fromRotation(rot)).multiply(Mat3.fromScale(s));
	}
	right() { return new Vec2(this.m[0], this.m[1]); }
	up() { return new Vec2(this.m[3], this.m[4]); }
	forward() { return new Vec2(this.m[6], this.m[7]); }
	translation(v) {
		if (v !== undefined) { this.m[6] = v.x; this.m[7] = v.y; return this; }
		return new Vec2(this.m[6], this.m[7]);
	}
	angle() { return Math.atan2(this.m[1], this.m[0]) * rad2deg; }
	add(f) { return new Mat3(this.m.map((v, i) => v + f.m[i])); }
	subtract(f) { return new Mat3(this.m.map((v, i) => v - f.m[i])); }
	multiply(f) {
		const a = this.m;
		if (typeof f === 'number') return new Mat3(a.map(v => v * f));
		if (f instanceof Mat3) {
			const b = f.m, r = new Array(9);
			for (let c = 0; c < 3; c++)
				for (let rr = 0; rr < 3; rr++)
					r[c * 3 + rr] = a[rr] * b[c * 3] + a[3 + rr] * b[c * 3 + 1] + a[6 + rr] * b[c * 3 + 2];
			return new Mat3(r);
		}
		return new Vec3(a[0] * f.x + a[3] * f.y + a[6] * f.z, a[1] * f.x + a[4] * f.y + a[7] * f.z,
			a[2] * f.x + a[5] * f.y + a[8] * f.z);
	}
	translate(v) { return this.multiply(Mat3.fromTranslation(v)); }
	rotate(angle) { return this.multiply(Mat3.fromRotation(angle)); }
	scale(v) { return this.multiply(Mat3.fromScale(v)); }
	transformPoint(v) { const r = this.multiply(new Vec3(v.x, v.y, 1)); return new Vec2(r.x, r.y); }
	transformDirection(v) { const r = this.multiply(new Vec3(v.x, v.y, 0)); return new Vec2(r.x, r.y); }
	transpose() { const a = this.m; return new Mat3([a[0], a[3], a[6], a[1], a[4], a[7], a[2], a[5], a[8]]); }
	determinant() {
		const a = this.m;
		return a[0] * (a[4] * a[8] - a[7] * a[5]) - a[3] * (a[1] * a[8] - a[7] * a[2]) + a[6] * (a[1] * a[5] - a[4] * a[2]);
	}
	inverse() {
		const a = this.m, d = this.determinant();
		if (d === 0) return new Mat3();
		const i = 1 / d;
		return new Mat3([
			(a[4] * a[8] - a[5] * a[7]) * i, (a[2] * a[7] - a[1] * a[8]) * i, (a[1] * a[5] - a[2] * a[4]) * i,
			(a[5] * a[6] - a[3] * a[8]) * i, (a[0] * a[8] - a[2] * a[6]) * i, (a[2] * a[3] - a[0] * a[5]) * i,
			(a[3] * a[7] - a[4] * a[6]) * i, (a[1] * a[6] - a[0] * a[7]) * i, (a[0] * a[4] - a[1] * a[3]) * i]);
	}
	decompose() {
		return { translation: this.translation(), rotation: this.angle(),
			scale: new Vec2(this.right().length(), this.up().length()) };
	}
	copy() { return new Mat3(this); }
	equals(f) { return f instanceof Mat3 && this.m.every((v, i) => Math.abs(v - f.m[i]) < _Epsilon); }
	toString() { return this.m.join(' '); }
	toConfigString() { return this.toString(); }
}

class Mat4 {
	constructor(arg) {
		if (arg instanceof Mat4) this.m = arg.m.slice();
		else if (Array.isArray(arg) && arg.length === 16) this.m = arg.slice();
		else if (typeof arg === 'string' && _splitFloats(arg).length === 16) this.m = _splitFloats(arg);
		else this.m = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1];
	}
	static identity() { return new Mat4(); }
	static fromTranslation(v) {
		const r = new Mat4(); r.m[12] = v.x; r.m[13] = v.y; r.m[14] = typeof v.z === 'number' ? v.z : 0; return r;
	}
	static fromScale(v) {
		const r = new Mat4(); r.m[0] = _comp(v, 'x'); r.m[5] = _comp(v, 'y'); r.m[10] = _comp(v, 'z'); return r;
	}
	static fromRotation(angle, axis) {
		const a = angle * deg2rad, c = Math.cos(a), s = Math.sin(a), t = 1 - c;
		const n = axis.normalize(), x = n.x, y = n.y, z = n.z;
		return new Mat4([
			t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0,
			t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0,
			t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0,
			0, 0, 0, 1]);
	}
	static fromEuler(x, y, z) {
		if (typeof x === 'object') { z = x.z; y = x.y; x = x.x; }
		return Mat4.fromRotation(z || 0, new Vec3(0, 0, 1))
			.multiply(Mat4.fromRotation(y || 0, new Vec3(0, 1, 0)))
			.multiply(Mat4.fromRotation(x || 0, new Vec3(1, 0, 0)));
	}
	static fromBasis(right, up, forward) {
		return new Mat4([right.x, right.y, right.z, 0, up.x, up.y, up.z, 0, forward.x, forward.y, forward.z, 0, 0, 0, 0, 1]);
	}
	static lookAt(eye, center, up) {
		const f = center.subtract(eye).normalize(), s = f.cross(up).normalize(), u = s.cross(f);
		return new Mat4([s.x, u.x, -f.x, 0, s.y, u.y, -f.y, 0, s.z, u.z, -f.z, 0,
			-s.dot(eye), -u.dot(eye), f.dot(eye), 1]);
	}
	static compose(t, rot, s) {
		return Mat4.fromTranslation(t).multiply(Mat4.fromEuler(rot)).multiply(Mat4.fromScale(s));
	}
	translation(v) {
		if (v !== undefined) {
			this.m[12] = v.x; this.m[13] = v.y; this.m[14] = typeof v.z === 'number' ? v.z : 0;
			return this;
		}
		return new Vec3(this.m[12], this.m[13], this.m[14]);
	}
	right() { return new Vec3(this.m[0], this.m[1], this.m[2]); }
	up() { return new Vec3(this.m[4], this.m[5], this.m[6]); }
	forward() { return new Vec3(this.m[8], this.m[9], this.m[10]); }
	add(f) { return new Mat4(this.m.map((v, i) => v + f.m[i])); }
	subtract(f) { return new Mat4(this.m.map((v, i) => v - f.m[i])); }
	multiply(f) {
		const a = this.m;
		if (typeof f === 'number') return new Mat4(a.map(v => v * f));
		if (f instanceof Mat4) {
			const b = f.m, r = new Array(16);
			for (let c = 0; c < 4; c++)
				for (let rr = 0; rr < 4; rr++)
					r[c * 4 + rr] = a[rr] * b[c * 4] + a[4 + rr] * b[c * 4 + 1] + a[8 + rr] * b[c * 4 + 2] + a[12 + rr] * b[c * 4 + 3];
			return new Mat4(r);
		}
		const w = typeof f.w === 'number' ? f.w : 1;
		return new Vec4(
			a[0] * f.x + a[4] * f.y + a[8] * f.z + a[12] * w, a[1] * f.x + a[5] * f.y + a[9] * f.z + a[13] * w,
			a[2] * f.x + a[6] * f.y + a[10] * f.z + a[14] * w, a[3] * f.x + a[7] * f.y + a[11] * f.z + a[15] * w);
	}
	translate(v) { return this.multiply(Mat4.fromTranslation(v)); }
	rotate(angle, axis) { return this.multiply(Mat4.fromRotation(angle, axis)); }
	scale(v) { return this.multiply(Mat4.fromScale(v)); }
	transformPoint(v) { const r = this.multiply(new Vec4(v.x, v.y, v.z || 0, 1)); return new Vec3(r.x, r.y, r.z); }
	transformDirection(v) { const r = this.multiply(new Vec4(v.x, v.y, v.z || 0, 0)); return new Vec3(r.x, r.y, r.z); }
	transpose() {
		const a = this.m, r = new Array(16);
		for (let c = 0; c < 4; c++) for (let rr = 0; rr < 4; rr++) r[c * 4 + rr] = a[rr * 4 + c];
		return new Mat4(r);
	}
	_cofactors() {
		const m = this.m;
		const b = [
			m[0] * m[5] - m[1] * m[4], m[0] * m[6] - m[2] * m[4], m[0] * m[7] - m[3] * m[4],
			m[1] * m[6] - m[2] * m[5], m[1] * m[7] - m[3] * m[5], m[2] * m[7] - m[3] * m[6],
			m[8] * m[13] - m[9] * m[12], m[8] * m[14] - m[10] * m[12], m[8] * m[15] - m[11] * m[12],
			m[9] * m[14] - m[10] * m[13], m[9] * m[15] - m[11] * m[13], m[10] * m[15] - m[11] * m[14]];
		return b;
	}
	determinant() {
		const b = this._cofactors();
		return b[0] * b[11] - b[1] * b[10] + b[2] * b[9] + b[3] * b[8] - b[4] * b[7] + b[5] * b[6];
	}
	inverse() {
		const m = this.m, b = this._cofactors();
		const d = b[0] * b[11] - b[1] * b[10] + b[2] * b[9] + b[3] * b[8] - b[4] * b[7] + b[5] * b[6];
		if (d === 0) return new Mat4();
		const i = 1 / d;
		return new Mat4([
			(m[5] * b[11] - m[6] * b[10] + m[7] * b[9]) * i, (m[2] * b[10] - m[1] * b[11] - m[3] * b[9]) * i,
			(m[13] * b[5] - m[14] * b[4] + m[15] * b[3]) * i, (m[10] * b[4] - m[9] * b[5] - m[11] * b[3]) * i,
			(m[6] * b[8] - m[4] * b[11] - m[7] * b[7]) * i, (m[0] * b[11] - m[2] * b[8] + m[3] * b[7]) * i,
			(m[14] * b[2] - m[12] * b[5] - m[15] * b[1]) * i, (m[8] * b[5] - m[10] * b[2] + m[11] * b[1]) * i,
			(m[4] * b[10] - m[5] * b[8] + m[7] * b[6]) * i, (m[1] * b[8] - m[0] * b[10] - m[3] * b[6]) * i,
			(m[12] * b[4] - m[13] * b[2] + m[15] * b[0]) * i, (m[9] * b[2] - m[8] * b[4] - m[11] * b[0]) * i,
			(m[5] * b[7] - m[4] * b[9] - m[6] * b[6]) * i, (m[0] * b[9] - m[1] * b[7] + m[2] * b[6]) * i,
			(m[13] * b[1] - m[12] * b[3] - m[14] * b[0]) * i, (m[8] * b[3] - m[9] * b[1] + m[10] * b[0]) * i]);
	}
	extractEuler() {
		const m = this.m, sy = -m[2];
		if (Math.abs(sy) < 0.99999) {
			return new Vec3(Math.atan2(m[6], m[10]) * rad2deg, Math.asin(sy) * rad2deg, Math.atan2(m[1], m[0]) * rad2deg);
		}
		return new Vec3(Math.atan2(-m[9], m[5]) * rad2deg, Math.asin(sy) * rad2deg, 0);
	}
	normalMatrix() {
		const a = this.inverse().transpose().m;
		return new Mat3([a[0], a[1], a[2], a[4], a[5], a[6], a[8], a[9], a[10]]);
	}
	decompose() {
		const s = new Vec3(this.right().length(), this.up().length(), this.forward().length());
		const r = new Mat4(this.m);
		for (let i = 0; i < 3; i++) { r.m[i] /= s.x || 1; r.m[4 + i] /= s.y || 1; r.m[8 + i] /= s.z || 1; }
		return { translation: this.translation(), rotation: r.extractEuler(), scale: s };
	}
	copy() { return new Mat4(this); }
	equals(f) { return f instanceof Mat4 && this.m.every((v, i) => Math.abs(v - f.m[i]) < _Epsilon); }
	toString() { return this.m.join(' '); }
	toConfigString() { return this.toString(); }
}

class MediaPlaybackEvent {
	static PLAYBACK_STOPPED = 0;
	static PLAYBACK_PLAYING = 1;
	static PLAYBACK_PAUSED = 2;
}

function _stringifyAdapter(key, value) {
	if (value && value.toConfigString) return value.toConfigString();
	return value;
}

this._Internal = {
	updateScriptProperties(script, vars) {
		if (typeof vars === 'string') vars = JSON.parse(vars);
		const sp = script.scriptProperties;
		for (const key of Object.keys(vars)) {
			if (!Object.prototype.hasOwnProperty.call(sp, key)) continue;
			sp[key] = sp[key] instanceof Vec3 ? new Vec3(vars[key]) : vars[key];
		}
	},
	convertUserProperties(p) {
		if (typeof p === 'string') p = JSON.parse(p);
		const r = {};
		for (const k in p) {
			const e = p[k];
			if (e === null || typeof e !== 'object') r[k] = e;
			else if (e.type === 'color') r[k] = new Vec3(e.value);
			else if (e.type === 'usershortcut') r[k] = { isbound: e.isbound, commandtype: e.commandtype, file: e.file };
			else r[k] = e.value;
		}
		return r;
	},
	stringifyConfig(obj) {
		return JSON.stringify(obj, _stringifyAdapter);
	}
};

this.createScriptProperties = function () {
	const vars = {};
	let order = 0;
	const config = (o, extra) => Object.assign({ order: order++, label: o.label }, extra);
	const builder = {
		addSlider(o) {
			vars[o.name] = o.value;
			vars[o.name + '_config'] = config(o, { min: o.min, max: o.max, mode: o.integer === true ? 'int' : undefined });
			return builder;
		},
		addCheckbox(o) { vars[o.name] = o.value; vars[o.name + '_config'] = config(o); return builder; },
		addText(o) { vars[o.name] = o.value; vars[o.name + '_config'] = config(o); return builder; },
		addTextInput(o) { return builder.addText(o); },
		addColor(o) { vars[o.name] = o.value; vars[o.name + '_config'] = config(o); return builder; },
		addFile(o) { vars[o.name] = o.value; vars[o.name + '_config'] = config(o); return builder; },
		addDirectory(o) { vars[o.name] = o.value; vars[o.name + '_config'] = config(o); return builder; },
		addCombo(o) {
			const opts = o.options || [];
			vars[o.name] = o.value !== undefined ? o.value : (opts.length ? opts[0].value : undefined);
			vars[o.name + '_config'] = config(o, { options: opts, mode: 'combo' });
			return builder;
		},
		finish() { return vars; }
	};
	return builder;
};
