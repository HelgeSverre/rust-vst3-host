//! Example-local DSP shared by the trance demos (`trance_timeline_demo`, `trance_timeline_gui`):
//! a band-limited ping-pong delay, a Dattorro plate reverb, and a 3-band parametric EQ.
//!
//! This is demo garnish, not library API — the `vst3-host` crate stays a hosting library.
//! Include it from an example with `#[path = "dsp/mod.rs"] mod dsp;`.

/// A simple circular delay line. `push` writes one sample; `tap(k)` reads the sample written
/// `k` pushes ago (`k >= 1`).
struct Delay {
    buf: Vec<f32>,
    w: usize,
}

impl Delay {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            w: 0,
        }
    }

    fn push(&mut self, x: f32) {
        self.buf[self.w] = x;
        self.w = (self.w + 1) % self.buf.len();
    }

    fn tap(&self, k: usize) -> f32 {
        let n = self.buf.len();
        debug_assert!(k >= 1 && k <= n);
        self.buf[(self.w + n - k) % n]
    }

    /// Fractional tap with linear interpolation, for modulated delay lengths.
    fn tap_frac(&self, k: f32) -> f32 {
        let i = k.floor();
        let frac = k - i;
        let i = (i as usize).clamp(1, self.buf.len() - 1);
        self.tap(i) * (1.0 - frac) + self.tap(i + 1) * frac
    }
}

/// A Schroeder lattice allpass over a delay line: dense echoes, flat magnitude response.
struct Allpass {
    delay: Delay,
    len: usize,
    g: f32,
}

impl Allpass {
    fn new(len: usize, g: f32) -> Self {
        Self {
            delay: Delay::new(len + 64), // headroom for modulation excursion at high sample rates
            len,
            g,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let z = self.delay.tap(self.len);
        let v = x + self.g * z;
        let y = z - self.g * v;
        self.delay.push(v);
        y
    }

    /// Like `process`, but reading at a modulated (fractional) length.
    fn process_mod(&mut self, x: f32, len: f32) -> f32 {
        let z = self.delay.tap_frac(len);
        let v = x + self.g * z;
        let y = z - self.g * v;
        self.delay.push(v);
        y
    }
}

/// Stateful tempo-synced ping-pong delay.
///
/// The mono-summed input feeds the **left line only** and the feedback crosses channels, so
/// echoes truly alternate L→R→L. (Feeding both lines from a mono-ish synth makes the two lines
/// identical — mono echoes, no ping-pong.) The loop is band-limited like an analog DDL: a
/// one-pole low-pass darkens each repeat and a one-pole high-pass keeps the lows from piling up.
pub struct PingPong {
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    w: usize,
    /// Current tap length in samples (≤ buffer size); changeable live via [`set_delay`].
    delay: usize,
    /// Feedback-path low-pass (damping) states.
    damp: [f32; 2],
    /// Feedback-path low-cut states (low-pass that gets subtracted).
    locut: [f32; 2],
    damp_coef: f32,
    locut_coef: f32,
}

impl PingPong {
    /// `samples` sizes the buffer and is the initial (and maximum) delay length.
    pub fn new(samples: usize, sr: f32) -> Self {
        let n = samples.max(1);
        Self {
            buf_l: vec![0.0; n],
            buf_r: vec![0.0; n],
            w: 0,
            delay: n,
            damp: [0.0; 2],
            locut: [0.0; 2],
            damp_coef: onepole_coef(5500.0, sr), // repeats darken above ~5.5 kHz
            locut_coef: onepole_coef(180.0, sr), // ...and thin out below ~180 Hz
        }
    }

    /// Change the tap length live (clamped to the buffer). Stepped changes click a little —
    /// like retuning a hardware DDL — which is fine for switching musical divisions.
    #[allow(dead_code)] // used by the GUI's delay TIME knob; the offline demo sizes at new()
    pub fn set_delay(&mut self, samples: usize) {
        self.delay = samples.clamp(1, self.buf_l.len());
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], feedback: f32, wet: f32) {
        let n = self.buf_l.len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let read = (self.w + n - self.delay) % n;
            let (dl, dr) = (self.buf_l[read], self.buf_r[read]);
            let dry = 0.5 * (*l + *r);
            // Write heads: input enters the left line; feedback crosses L↔R.
            let (wl, wr) = (dry + dr * feedback, dl * feedback);
            self.damp[0] += self.damp_coef * (wl - self.damp[0]);
            self.locut[0] += self.locut_coef * (self.damp[0] - self.locut[0]);
            self.buf_l[self.w] = self.damp[0] - self.locut[0];
            self.damp[1] += self.damp_coef * (wr - self.damp[1]);
            self.locut[1] += self.locut_coef * (self.damp[1] - self.locut[1]);
            self.buf_r[self.w] = self.damp[1] - self.locut[1];
            *l += dl * wet;
            *r += dr * wet;
            self.w = (self.w + 1) % n;
        }
    }
}

/// The reference sample rate Dattorro's delay lengths are specified at.
const DATTORRO_SR: f64 = 29_761.0;

/// A plate reverb after Jon Dattorro, "Effect Design, Part 1" (JAES 1997) — Griesinger's
/// Lexicon figure-of-eight topology, the same feedback-allpass-loop family that Valhalla-style
/// reverbs grow out of. Input diffusion (4 series allpasses) feeds a two-branch tank whose
/// branches cross-feed each other; each branch is a *modulated* allpass (the slow ±12-sample
/// wobble is what makes the tail lush instead of metallic), a long delay, a damping low-pass,
/// and another allpass + delay. Output is the paper's 7-taps-per-side matrix. Two trance
/// touches on top of the paper: the wet signal is low-cut (~250 Hz) so the reverb never muddies
/// the bassline, and pre-delay keeps the dry transient articulate in front of the wash.
pub struct PlateReverb {
    /// Tank feedback, 0..1 — roughly "decay time" (0.5 = paper default, 0.8+ = trance hall).
    pub decay: f32,
    /// High-frequency loss in the tank, 0..1 (0 = bright forever, 1 = dark instantly).
    pub damping: f32,
    predelay: Delay,
    predelay_len: usize,
    /// Input bandwidth one-pole low-pass (tames the very top going into the tank).
    in_lp: f32,
    in_lp_coef: f32,
    in_diffusion: [Allpass; 4],
    /// Per branch: modulated input allpass, first long delay, damping state, second allpass,
    /// second long delay, and the branch's feedback value (read by the *other* branch).
    mod_ap: [Allpass; 2],
    tank_d1: [Delay; 2],
    tank_lp: [f32; 2],
    tank_ap: [Allpass; 2],
    tank_d2: [Delay; 2],
    fb: [f32; 2],
    mod_ap_len: [f32; 2],
    lfo_phase: f32,
    lfo_inc: f32,
    excursion: f32,
    /// Low-cut on the wet output (one-pole low-pass states, subtracted).
    wet_locut: [f32; 2],
    wet_locut_coef: f32,
    /// Output tap offsets (scaled to the running sample rate), per side.
    taps_l: [usize; 7],
    taps_r: [usize; 7],
}

/// Output tap signs from the paper, common to both sides.
const TAP_SIGNS: [f32; 7] = [1.0, 1.0, -1.0, 1.0, -1.0, -1.0, -1.0];

impl PlateReverb {
    pub fn new(sr: f64, predelay_ms: f64) -> Self {
        let scale = |n: f64| ((n * sr / DATTORRO_SR).round() as usize).max(1);
        let predelay_len = scale(predelay_ms / 1000.0 * DATTORRO_SR).max(1);
        Self {
            decay: 0.5,
            damping: 0.3,
            predelay: Delay::new(predelay_len),
            predelay_len,
            in_lp: 0.0,
            in_lp_coef: onepole_coef(10_000.0, sr as f32),
            // Input diffusion delays/gains straight from the paper.
            in_diffusion: [
                Allpass::new(scale(142.0), 0.75),
                Allpass::new(scale(107.0), 0.75),
                Allpass::new(scale(379.0), 0.625),
                Allpass::new(scale(277.0), 0.625),
            ],
            mod_ap: [
                Allpass::new(scale(672.0), 0.70),
                Allpass::new(scale(908.0), 0.70),
            ],
            tank_d1: [Delay::new(scale(4453.0)), Delay::new(scale(4217.0))],
            tank_lp: [0.0; 2],
            tank_ap: [
                Allpass::new(scale(1800.0), 0.50),
                Allpass::new(scale(2656.0), 0.50),
            ],
            tank_d2: [Delay::new(scale(3720.0)), Delay::new(scale(3163.0))],
            fb: [0.0; 2],
            mod_ap_len: [scale(672.0) as f32, scale(908.0) as f32],
            lfo_phase: 0.0,
            lfo_inc: std::f32::consts::TAU * 0.9 / sr as f32, // ~0.9 Hz wobble
            excursion: (12.0 * sr / DATTORRO_SR) as f32,
            wet_locut: [0.0; 2],
            wet_locut_coef: onepole_coef(250.0, sr as f32),
            taps_l: [
                scale(266.0),
                scale(2974.0),
                scale(1913.0),
                scale(1996.0),
                scale(1990.0),
                scale(187.0),
                scale(1066.0),
            ],
            taps_r: [
                scale(353.0),
                scale(3627.0),
                scale(1228.0),
                scale(2673.0),
                scale(2111.0),
                scale(335.0),
                scale(121.0),
            ],
        }
    }

    /// Process a stereo pair in place, mixing `wet` (0..1) of reverb onto the dry signal.
    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], wet: f32) {
        let damp = self.damping.clamp(0.0, 1.0) * 0.8; // keep some top even at full damping
        let decay = self.decay.clamp(0.0, 0.98);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            // Mono in: pre-delay, then gentle input band-limit, then the 4 diffusion allpasses.
            let x = self.predelay.tap(self.predelay_len);
            self.predelay.push(0.5 * (*l + *r));
            self.in_lp += self.in_lp_coef * (x - self.in_lp);
            let mut x = self.in_lp;
            for ap in &mut self.in_diffusion {
                x = ap.process(x);
            }

            // Tank: two branches in a figure-of-eight, each fed by the other's tail.
            let (s, c) = self.lfo_phase.sin_cos();
            let lfo = [s, c]; // quadrature, so the two branches never wobble in step
            self.lfo_phase += self.lfo_inc;
            if self.lfo_phase > std::f32::consts::TAU {
                self.lfo_phase -= std::f32::consts::TAU;
            }
            for (b, &lfo_b) in lfo.iter().enumerate() {
                let mut t = x + self.fb[1 - b] * decay;
                let len = self.mod_ap_len[b] + self.excursion * lfo_b;
                t = self.mod_ap[b].process_mod(t, len);
                let d1 = &mut self.tank_d1[b];
                let delayed = d1.tap(d1.buf.len()); // read before write: exact full-length delay
                d1.push(t);
                self.tank_lp[b] += (1.0 - damp) * (delayed - self.tank_lp[b]);
                t = self.tank_lp[b] * decay;
                t = self.tank_ap[b].process(t);
                let d2 = &mut self.tank_d2[b];
                self.fb[b] = d2.tap(d2.buf.len());
                d2.push(t);
            }

            // The paper's output matrix: 7 taps per side, mirrored across the branches
            // (left listens mostly to branch 1, right to branch 0).
            let tap_set = |taps: &[usize; 7], a: usize, this: &Self| {
                let b = 1 - a;
                TAP_SIGNS[0] * this.tank_d1[b].tap(taps[0])
                    + TAP_SIGNS[1] * this.tank_d1[b].tap(taps[1])
                    + TAP_SIGNS[2] * this.tank_ap[b].delay.tap(taps[2])
                    + TAP_SIGNS[3] * this.tank_d2[b].tap(taps[3])
                    + TAP_SIGNS[4] * this.tank_d1[a].tap(taps[4])
                    + TAP_SIGNS[5] * this.tank_ap[a].delay.tap(taps[5])
                    + TAP_SIGNS[6] * this.tank_d2[a].tap(taps[6])
            };
            let mut wl = 0.6 * tap_set(&self.taps_l, 0, self);
            let mut wr = 0.6 * tap_set(&self.taps_r, 1, self);

            // Trance hygiene: low-cut the wet so the tail never sits on the bassline.
            self.wet_locut[0] += self.wet_locut_coef * (wl - self.wet_locut[0]);
            wl -= self.wet_locut[0];
            self.wet_locut[1] += self.wet_locut_coef * (wr - self.wet_locut[1]);
            wr -= self.wet_locut[1];

            *l += wl * wet;
            *r += wr * wet;
        }
    }
}

/// One band of the 3-band EQ: center/corner frequency, boost/cut, and bandwidth.
#[derive(Clone, Copy)]
pub struct EqBand {
    pub hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

/// A biquad in transposed direct form II, with per-channel state.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z: [[f32; 2]; 2],
}

impl Biquad {
    /// Normalize RBJ cookbook coefficients (divides through by `a0`).
    fn from_rbj(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z: [[0.0; 2]; 2],
        }
    }

    fn tick(&mut self, ch: usize, x: f32) -> f32 {
        let z = &mut self.z[ch];
        let y = self.b0 * x + z[0];
        z[0] = self.b1 * x - self.a1 * y + z[1];
        z[1] = self.b2 * x - self.a2 * y;
        y
    }
}

/// A simple stereo 3-band parametric EQ: low shelf, mid peak, high shelf — RBJ cookbook
/// biquads (Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad filter
/// coefficients"), the same curves every DAW channel EQ ships.
pub struct ThreeBandEq {
    bands: [Biquad; 3],
}

impl ThreeBandEq {
    /// `low` is a low shelf, `mid` a peaking band, `high` a high shelf.
    pub fn new(sr: f32, low: EqBand, mid: EqBand, high: EqBand) -> Self {
        Self {
            bands: [
                Self::shelf(sr, low, false),
                Self::peak(sr, mid),
                Self::shelf(sr, high, true),
            ],
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for band in &mut self.bands {
            for s in left.iter_mut() {
                *s = band.tick(0, *s);
            }
            for s in right.iter_mut() {
                *s = band.tick(1, *s);
            }
        }
    }

    fn peak(sr: f32, band: EqBand) -> Biquad {
        let a = 10f32.powf(band.gain_db / 40.0);
        let w0 = std::f32::consts::TAU * band.hz / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * band.q.max(0.05));
        Biquad::from_rbj(
            [1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a],
            [1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a],
        )
    }

    fn shelf(sr: f32, band: EqBand, high: bool) -> Biquad {
        let a = 10f32.powf(band.gain_db / 40.0);
        let w0 = std::f32::consts::TAU * band.hz / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * band.q.max(0.05));
        let sq = 2.0 * a.sqrt() * alpha;
        // The high shelf is the low shelf with the sign of every `cos` term flipped.
        let c = if high { -cos } else { cos };
        let b = [
            a * ((a + 1.0) - (a - 1.0) * c + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c) * if high { -1.0 } else { 1.0 },
            a * ((a + 1.0) - (a - 1.0) * c - sq),
        ];
        let a_coefs = [
            (a + 1.0) + (a - 1.0) * c + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * c) * if high { -1.0 } else { 1.0 },
            (a + 1.0) + (a - 1.0) * c - sq,
        ];
        Biquad::from_rbj(b, a_coefs)
    }
}

/// One-pole smoothing coefficient for a given corner frequency.
fn onepole_coef(fc: f32, sr: f32) -> f32 {
    1.0 - (-std::f32::consts::TAU * fc / sr).exp()
}

/// A trance kick voice: a fast exponential pitch drop (a few hundred Hz collapsing onto a
/// ~50 Hz body within ~10–20 ms — that drop *is* the click) into a tanh-saturated sine, with
/// an exponential amplitude decay. The saturation is what separates a trance kick from a
/// soft 909 thud: it packs the body with harmonics so it knocks on small speakers too.
/// Streaming and re-triggerable, so it works both in a live audio callback and offline.
///
/// The shaping knobs (all normalized 0..1) are deliberately coarse but musical:
/// - `punch`: pitch-drop height/speed *and* saturation drive — more punch = clickier, denser.
/// - `decay`: amplitude tail, ~35 ms (tick) .. ~285 ms (boomy).
/// - `level`: output gain.
pub struct Kick {
    sr: f32,
    phase: f32,
    t: f32,
    active: bool,
}

impl Kick {
    pub fn new(sr: f32) -> Self {
        Self {
            sr,
            phase: 0.0,
            t: 0.0,
            active: false,
        }
    }

    pub fn trigger(&mut self) {
        self.phase = 0.0;
        self.t = 0.0;
        self.active = true;
    }

    /// Render the (remaining) kick additively into a stereo pair.
    pub fn process(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        level: f32,
        punch: f32,
        decay: f32,
    ) {
        if !self.active {
            return;
        }
        let f_end = 50.0;
        let f_start = 120.0 + 300.0 * punch;
        let pitch_tau = 0.022 - 0.014 * punch; // 8..22 ms — the drop is the click
        let amp_tau = 0.035 + 0.25 * decay;
        let drive = 1.5 + 2.5 * punch;
        let norm = 1.0 / drive.tanh(); // unity peak regardless of drive
        let gain = level * 0.85;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let a = (-self.t / amp_tau).exp();
            if a < 1e-3 {
                self.active = false;
                break;
            }
            let freq = f_end + (f_start - f_end) * (-self.t / pitch_tau).exp();
            self.phase += std::f32::consts::TAU * freq / self.sr;
            let s = (self.phase.sin() * a * drive).tanh() * norm * gain;
            *l += s;
            *r += s;
            self.t += 1.0 / self.sr;
        }
    }
}

/// Four-on-the-floor sidechain pump applied in place: at every beat the bus gain dips by
/// `depth` and recovers exponentially — the classic trance mix-bus duck, as if keyed by the
/// kick. Apply to the synth bus *before* adding the kick so the kick itself doesn't pump.
// This module is compiled once per example; the kick pair is only used by the offline demo.
#[allow(dead_code)]
pub fn sidechain_duck(left: &mut [f32], right: &mut [f32], sr: f64, bpm: f64, depth: f32) {
    let samples_per_beat = sr * 60.0 / bpm;
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = (i as f64 % samples_per_beat) / sr; // seconds since the last beat
        let g = 1.0 - depth * (-(t / 0.085)).exp() as f32;
        *l *= g;
        *r *= g;
    }
}
