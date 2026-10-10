import { AbsoluteFill, OffthreadVideo, Sequence, continueRender, delayRender, interpolate, staticFile, useCurrentFrame } from 'remotion';
import { useEffect, useState } from 'react';

export const fps = 30;

const seconds = (count: number) => count * fps;

// Timeline in seconds. Clip offsets trim the raw recordings in public/clips.
const timeline = [
  { kind: 'card', duration: 4, title: 'The rate you promise is the rate they get.', body: 'Firmament, firm FX for stablecoin payments on Solana' },
  { kind: 'card', duration: 7, title: 'USDC: $465M. EURC: $0.7M.', body: 'Solana DEX liquidity, Jupiter, Oct 2026. Any app promising a euro amount through a route is guessing.' },
  { kind: 'clip', duration: 40, src: 'clips/pay.mov', from: 0, caption: 'Exact-output quote, cost held for 30 seconds, one wallet confirmation, atomic delivery' },
  { kind: 'clip', duration: 14, src: 'clips/receive.mov', from: 2, caption: 'A fresh wallet received exactly 1 EURC' },
  { kind: 'clip', duration: 18, src: 'clips/maker.mov', from: 2, caption: "The maker's desk: inventory, every settlement, the ledger" },
  { kind: 'clip', duration: 14, src: 'clips/solscan.mov', from: 2, caption: 'On chain: lock, lock, redeem, redeem' },
  { kind: 'card', duration: 6, title: 'Firmament', body: 'github.com/azeemshaik025/firmament' }
] as const;

export const demoDurationInFrames = seconds(timeline.reduce((total, scene) => total + scene.duration, 0));

const paper = '#f6f5f1';
const ink = '#1d2130';
const muted = '#6b6e78';
const accent = '#e3b04b';
const display = "'Cabinet Grotesk', 'IBM Plex Sans', 'Helvetica Neue', sans-serif";
const body = "'IBM Plex Sans', 'Helvetica Neue', sans-serif";

const fontFaces = `
@font-face { font-family: 'Cabinet Grotesk'; font-weight: 500; src: url('${staticFile('fonts/cabinet-grotesk-500.woff2')}') format('woff2'); }
@font-face { font-family: 'IBM Plex Sans'; font-weight: 400 700; src: url('${staticFile('fonts/ibm-plex-sans-latin.woff2')}') format('woff2'); }
`;

function useFonts() {
  const [handle] = useState(() => delayRender('fonts'));
  useEffect(() => {
    Promise.all([document.fonts.load("500 96px 'Cabinet Grotesk'"), document.fonts.load("400 40px 'IBM Plex Sans'")])
      .catch(() => undefined)
      .finally(() => continueRender(handle));
  }, [handle]);
}

function Mark() {
  return (
    <div style={{ position: 'relative', width: 120, height: 48 }}>
      <div style={{ position: 'absolute', left: 0, right: 0, bottom: 6, height: 5, background: ink, borderRadius: 3 }} />
      <div style={{ position: 'absolute', left: 70, bottom: 10, width: 30, height: 30, borderRadius: 15, background: accent }} />
    </div>
  );
}

function Card({ title, body: copy }: { title: string; body: string }) {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 10], [0, 1], { extrapolateRight: 'clamp' });
  return (
    <AbsoluteFill style={{ background: paper, color: ink, justifyContent: 'center', padding: 140, opacity }}>
      <Mark />
      <h1 style={{ fontFamily: display, fontWeight: 500, fontSize: 104, letterSpacing: '-0.025em', lineHeight: 1.05, margin: '48px 0 0', maxWidth: 1500 }}>{title}</h1>
      <p style={{ fontFamily: body, fontSize: 40, lineHeight: 1.4, color: muted, marginTop: 36, maxWidth: 1300 }}>{copy}</p>
    </AbsoluteFill>
  );
}

function Clip({ src, from, caption }: { src: string; from: number; caption: string }) {
  return (
    <AbsoluteFill style={{ background: paper }}>
      <OffthreadVideo src={staticFile(src)} startFrom={seconds(from)} style={{ width: '100%', height: '100%', objectFit: 'contain' }} />
      <div style={{ position: 'absolute', left: 80, bottom: 56, padding: '18px 28px', background: ink, color: paper, fontSize: 34, fontFamily: body, borderRadius: 999 }}>
        {caption}
      </div>
    </AbsoluteFill>
  );
}

export const Demo = () => {
  useFonts();
  let from = 0;
  return (
    <AbsoluteFill style={{ background: paper }}>
      <style>{fontFaces}</style>
      {timeline.map((scene, index) => {
        const start = from;
        from += seconds(scene.duration);
        return (
          <Sequence key={index} from={start} durationInFrames={seconds(scene.duration)}>
            {scene.kind === 'card' ? <Card title={scene.title} body={scene.body} /> : <Clip src={scene.src} from={scene.from} caption={scene.caption} />}
          </Sequence>
        );
      })}
    </AbsoluteFill>
  );
};
