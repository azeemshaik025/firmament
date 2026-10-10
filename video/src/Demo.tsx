import { AbsoluteFill, OffthreadVideo, Sequence, interpolate, staticFile, useCurrentFrame } from 'remotion';

export const fps = 30;

const seconds = (count: number) => count * fps;

// Timeline in seconds; clip lengths are trimmed to the recordings in Task 11.
const timeline = [
  { kind: 'card', duration: 5, title: 'Firmament', body: 'The rate you promise is the rate they get.' },
  { kind: 'card', duration: 8, title: 'USDC: $465M. EURC: $0.7M.', body: 'Any app promising a euro amount through a Solana AMM is guessing.' },
  { kind: 'clip', duration: 60, src: 'clips/pay.mov', caption: 'Exact-output quote, locked 30s, one wallet confirmation' },
  { kind: 'clip', duration: 15, src: 'clips/receive.mov', caption: 'Recipient receives exactly 1.000000 EURC' },
  { kind: 'clip', duration: 20, src: 'clips/maker.mov', caption: "The maker's inventory, ledger and refill rail" },
  { kind: 'clip', duration: 15, src: 'clips/solscan.mov', caption: 'Both legs on chain: lock, lock, redeem, redeem' },
  { kind: 'card', duration: 7, title: 'Firm FX for stablecoin payments', body: 'github.com/azeemshaik025/firmament' }
] as const;

export const demoDurationInFrames = seconds(timeline.reduce((total, scene) => total + scene.duration, 0));

const font = "'IBM Plex Sans', 'Helvetica Neue', sans-serif";

function Card({ title, body }: { title: string; body: string }) {
  const frame = useCurrentFrame();
  const opacity = interpolate(frame, [0, 10], [0, 1], { extrapolateRight: 'clamp' });
  return (
    <AbsoluteFill style={{ background: '#0b0b0c', color: '#f4f1ea', justifyContent: 'center', padding: 120, fontFamily: font, opacity }}>
      <h1 style={{ fontSize: 96, margin: 0, lineHeight: 1.05 }}>{title}</h1>
      <p style={{ fontSize: 40, opacity: 0.8, marginTop: 32 }}>{body}</p>
    </AbsoluteFill>
  );
}

function Clip({ src, caption }: { src: string; caption: string }) {
  return (
    <AbsoluteFill style={{ background: '#0b0b0c' }}>
      <OffthreadVideo src={staticFile(src)} style={{ width: '100%', height: '100%', objectFit: 'contain' }} />
      <div style={{ position: 'absolute', left: 80, bottom: 60, padding: '16px 24px', background: 'rgba(0,0,0,0.7)', color: '#f4f1ea', fontSize: 36, fontFamily: font }}>
        {caption}
      </div>
    </AbsoluteFill>
  );
}

export const Demo = () => {
  let from = 0;
  return (
    <AbsoluteFill>
      {timeline.map((scene, index) => {
        const start = from;
        from += seconds(scene.duration);
        return (
          <Sequence key={index} from={start} durationInFrames={seconds(scene.duration)}>
            {scene.kind === 'card' ? <Card title={scene.title} body={scene.body} /> : <Clip src={scene.src} caption={scene.caption} />}
          </Sequence>
        );
      })}
    </AbsoluteFill>
  );
};
