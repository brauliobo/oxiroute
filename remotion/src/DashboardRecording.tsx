import type { CSSProperties, ReactNode } from 'react'
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from 'remotion'

export type DashboardRecordingProps = {
  scene: 'overview' | 'configuration'
}

const colors = {
  ink: '#e9eddf',
  muted: '#8f9788',
  dim: '#687161',
  line: '#30372d',
  lineBright: '#46503d',
  panel: '#151a14',
  deep: '#0e120d',
  lime: '#b6ff51',
  violet: '#b7a5ff',
  amber: '#ffcf70',
  coral: '#ff8b78',
}

const mono = 'IBM Plex Mono, SFMono-Regular, Consolas, monospace'
const serif = 'Georgia, Times New Roman, serif'

const panelStyle: CSSProperties = {
  border: `1px solid ${colors.lineBright}`,
  background: colors.panel,
}

export const DashboardRecording = ({ scene }: DashboardRecordingProps) => {
  const frame = useCurrentFrame()
  const { fps } = useVideoConfig()

  return (
    <AbsoluteFill style={{ background: colors.deep, color: colors.ink, fontFamily: 'Inter, system-ui, sans-serif' }}>
      <DashboardChrome scene={scene} frame={frame} />
      {scene === 'overview' ? <OverviewScene frame={frame} fps={fps} /> : <ConfigurationScene frame={frame} fps={fps} />}
    </AbsoluteFill>
  )
}

function DashboardChrome({ scene, frame }: { scene: DashboardRecordingProps['scene']; frame: number }) {
  const statusText = scene === 'overview' ? 'Telemetry live' : 'Revision-aware editor'
  const pulse = interpolate(frame, [0, 45, 90, 135, 179], [0.65, 1, 0.65, 1, 0.65], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' })

  return (
    <>
      <div style={{ height: 84, padding: '24px 42px 18px', borderBottom: `1px solid ${colors.line}`, display: 'flex', alignItems: 'end', justifyContent: 'space-between' }}>
        <div>
          <div style={eyebrowStyle}>Network control / telemetry</div>
          <div style={{ marginTop: 8, fontFamily: serif, fontSize: 41, letterSpacing: '-0.07em', lineHeight: 0.82 }}>OxiRoute</div>
          <div style={{ marginTop: 7, color: colors.lime, fontFamily: mono, fontSize: 11, letterSpacing: '0.06em' }}>{scene === 'overview' ? 'Runtime observatory' : 'Canonical configuration'}</div>
        </div>
        <div style={{ display: 'flex', alignItems: 'center', gap: 9, color: colors.ink, fontSize: 12 }}>
          <span style={{ width: 8, height: 8, borderRadius: '50%', background: colors.lime, boxShadow: `0 0 ${14 * pulse}px rgb(182 255 81 / 70%)` }} />
          {statusText}
        </div>
      </div>
      <div style={{ height: 50, padding: '0 42px', display: 'flex', alignItems: 'center', gap: 4, borderBottom: `1px solid ${colors.line}` }}>
        {['Overview', 'Statistics', 'Configuration'].map((label) => {
          const active = label === (scene === 'overview' ? 'Overview' : 'Configuration')
          return <div key={label} style={{ position: 'relative', height: 50, padding: '18px 15px 0', color: active ? colors.ink : colors.muted, fontFamily: mono, fontSize: 11, fontWeight: 700, letterSpacing: '0.08em', textTransform: 'uppercase' }}>{label}{active && <span style={{ position: 'absolute', right: 15, bottom: 0, left: 15, height: 2, background: colors.lime }} />}</div>
        })}
        <div style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 8, color: colors.muted, fontFamily: mono, fontSize: 10 }}><span style={{ color: colors.violet }}>TOKEN</span> held in page memory</div>
      </div>
    </>
  )
}

function OverviewScene({ frame, fps }: { frame: number; fps: number }) {
  const connections = Math.round(interpolate(frame, [0, 60, 120, 179], [18, 42, 27, 34], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' }))
  const traffic = (interpolate(frame, [0, 90, 179], [1.7, 4.4, 3.1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' })).toFixed(1)
  const rise = spring({ frame: Math.max(0, frame - 8), fps, config: { damping: 18, stiffness: 90 } })
  const streamLift = spring({ frame: Math.max(0, frame - 46), fps, config: { damping: 18, stiffness: 80 } })

  return (
    <div style={{ padding: '18px 42px 28px', opacity: rise }}>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(4, 1fr)', borderBottom: `1px solid ${colors.line}`, marginBottom: 18 }}>
        <Readout label="Active connections" value={String(connections)} />
        <Readout label="Traffic moved" value={`${traffic} MB`} />
        <Readout label="Host memory used" value="41.8%" />
        <Readout label="Uptime" value="02h 14m" monoValue />
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '1.35fr 1fr', gap: 14 }}>
        <Panel eyebrow="Aggregate network" title="Traffic" index="01" style={{ minHeight: 200 }}>
          <div style={{ display: 'flex', alignItems: 'end', justifyContent: 'space-between', margin: '22px 0 20px' }}>
            <div><div style={{ fontFamily: serif, fontSize: 60, letterSpacing: '-0.07em', lineHeight: 0.82 }}>{connections}</div><div style={captionStyle}>Connections active now</div></div>
            <div style={{ paddingLeft: 18, borderLeft: `1px solid ${colors.line}`, display: 'grid', gap: 4 }}><div style={labelStyle}>Lifetime accepted</div><strong style={{ fontSize: 18 }}>1,248</strong></div>
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8 }}><Direction label="IN" value="3.8 MB" /><Direction label="OUT" value="7.4 MB" outbound /></div>
        </Panel>
        <Panel eyebrow="Host pressure" title="Load / memory" index="02" style={{ minHeight: 200 }}>
          <div style={{ display: 'grid', gap: 13, margin: '23px 0' }}>
            <LoadRow label="01m" value="0.42" width={38} />
            <LoadRow label="05m" value="0.31" width={29} />
            <LoadRow label="15m" value="0.27" width={25} />
          </div>
          <div style={{ borderTop: `1px solid ${colors.line}`, paddingTop: 12 }}><div style={{ display: 'flex', justifyContent: 'space-between', color: colors.muted, fontSize: 11 }}><span>Memory used</span><strong style={{ color: colors.ink }}>1.7 GB / 4 GB</strong></div><div style={{ height: 7, marginTop: 9, background: '#30352d' }}><div style={{ width: '42%', height: '100%', background: colors.lime }} /></div></div>
        </Panel>
        <Panel eyebrow="OxiRoute process" title="Runtime" index="03" style={{ minHeight: 200 }}>
          <div style={{ margin: '20px 0 22px' }}><div style={{ color: colors.muted, fontFamily: mono, fontSize: 10, textTransform: 'uppercase' }}>CPU utilization</div><div style={{ marginTop: 5, color: colors.lime, fontFamily: serif, fontSize: 48, letterSpacing: '-0.06em', lineHeight: 0.85 }}>8.4%</div></div>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', borderBlock: `1px solid ${colors.line}` }}><Compact label="Resident" value="184 MB" /><Compact label="Threads" value="18" /><Compact label="Open files" value="42" /><Compact label="Retries" value="0" /></div>
        </Panel>
        <Panel eyebrow="Media plane" title="RTMP pulse" index="04" style={{ minHeight: 200 }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 18, margin: '17px 0 22px' }}><div style={{ position: 'relative', width: 66, height: 66, border: `1px solid ${colors.lineBright}`, borderRadius: '50%' }}><div style={{ position: 'absolute', inset: 11, border: `1px solid #78995b`, borderRadius: '50%' }} /><div style={{ position: 'absolute', inset: 28, borderRadius: '50%', background: colors.lime, boxShadow: `0 0 18px ${colors.lime}` }} /></div><div><div style={{ fontFamily: serif, fontSize: 50, letterSpacing: '-0.06em', lineHeight: 0.84 }}>1</div><div style={captionStyle}>Active stream</div></div></div>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1.25fr', borderBlock: `1px solid ${colors.line}` }}><Compact label="Publishers" value="1" /><Compact label="Viewers" value="6" /><Compact label="Media" value="8.2 MB" /></div>
        </Panel>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '1.05fr 1fr', gap: 14, marginTop: 14, transform: `translateY(${(1 - streamLift) * 12}px)`, opacity: streamLift }}>
        <div style={{ ...panelStyle, padding: '15px 18px' }}><div style={eyebrowStyle}>Bound surfaces</div><div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginTop: 13 }}><div><strong style={{ fontSize: 14 }}>web</strong><div style={{ color: colors.muted, fontFamily: mono, fontSize: 10, marginTop: 4 }}>HTTP / 127.0.0.1:8080</div></div><Status label="listening" color={colors.lime} /><div style={{ color: colors.muted, fontFamily: mono, fontSize: 11 }}>18 / unbounded</div></div></div>
        <div style={{ ...panelStyle, padding: '15px 18px' }}><div style={eyebrowStyle}>Origin readiness</div><div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginTop: 13 }}><div><strong style={{ fontSize: 14 }}>web</strong><div style={{ color: colors.muted, fontFamily: mono, fontSize: 10, marginTop: 4 }}>127.0.0.1:3000</div></div><Status label="healthy" color={colors.lime} /><div style={{ color: colors.muted, fontFamily: mono, fontSize: 11 }}>42 checks</div></div></div>
      </div>
    </div>
  )
}

function ConfigurationScene({ frame, fps }: { frame: number; fps: number }) {
  const editorLift = spring({ frame: Math.max(0, frame - 10), fps, config: { damping: 18, stiffness: 75 } })
  const review = interpolate(frame, [72, 100], [0, 1], { extrapolateLeft: 'clamp', extrapolateRight: 'clamp' })
  const saved = frame > 140

  return (
    <div style={{ padding: '18px 42px 28px', opacity: editorLift }}>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', marginBottom: 18, border: `1px solid ${colors.lineBright}`, background: colors.panel }}>
        <Revision label="Disk revision" value="0f4d...a92c" detail="Save precondition" />
        <Revision label="Active revision" value="0f4d...a92c" detail="Serving live traffic" />
        <Revision label="Draft state" value={saved ? 'Saved / active' : 'Unsaved changes'} detail={saved ? 'Candidate published' : 'Validation required'} accent={saved ? colors.lime : colors.amber} />
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '190px 1fr', gap: 14 }}>
        <div style={{ ...panelStyle, padding: 16, minHeight: 425 }}><div style={eyebrowStyle}>Objects</div><div style={{ display: 'grid', gap: 4, marginTop: 20 }}>{['General', 'Management', 'Statistics', 'Listeners', 'Upstream pools', 'HTTP services', 'RTMP services'].map((item, index) => <div key={item} style={{ padding: '10px 9px', color: index === 4 ? colors.ink : colors.muted, borderLeft: `2px solid ${index === 4 ? colors.lime : 'transparent'}`, background: index === 4 ? '#1e281b' : 'transparent', fontFamily: mono, fontSize: 10 }}>{item}<span style={{ display: 'block', marginTop: 3, color: colors.dim, fontSize: 9 }}>{index === 4 ? '2 servers' : index === 3 ? '3 listeners' : ' '}</span></div>)}</div></div>
        <div style={{ ...panelStyle, padding: 22, minHeight: 425 }}><div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'start', borderBottom: `1px solid ${colors.line}`, paddingBottom: 14 }}><div><div style={eyebrowStyle}>Origin group</div><div style={{ marginTop: 6, fontFamily: serif, fontSize: 31, letterSpacing: '-0.05em' }}>web</div></div><div style={{ color: colors.muted, fontFamily: mono, fontSize: 10 }}>Config.upstream_pools[0]</div></div><div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 14, marginTop: 18 }}><Field label="Stable name" value="web" /><Field label="Selection algorithm" value="round_robin" /><Field label="Server 1" value="origin-a" /><Field label="Endpoint" value="127.0.0.1:3000" /></div><div style={{ marginTop: 17, padding: 15, border: `1px solid ${colors.line}`, background: colors.deep }}><div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}><div style={eyebrowStyle}>Health check</div><Status label="enabled" color={colors.lime} /></div><div style={{ display: 'grid', gridTemplateColumns: 'repeat(3, 1fr)', gap: 12, marginTop: 15 }}><Field label="Probe" value="GET /healthz" compact /><Field label="Interval" value="5000 ms" compact /><Field label="Threshold" value="1 / 3" compact /></div></div><div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginTop: 18, padding: '11px 13px', border: `1px solid ${review > 0 ? colors.violet : colors.lineBright}`, background: review > 0 ? '#201b31' : colors.deep, opacity: review > 0 ? 1 : 0.72 }}><span style={{ color: review > 0 ? colors.violet : colors.muted, fontFamily: mono, fontSize: 10 }}>{review > 0 ? 'Candidate validated / preview ready' : 'Edit fields to enable server validation'}</span><span style={{ color: review > 0 ? colors.violet : colors.dim, fontFamily: mono, fontSize: 10 }}>{review > 0 ? 'REVIEW' : 'DRAFT'}</span></div></div>
      </div>
    </div>
  )
}

function Panel({ eyebrow, title, index, children, style }: { eyebrow: string; title: string; index: string; children: ReactNode; style?: CSSProperties }) {
  return <div style={{ ...panelStyle, padding: '17px 19px', ...style }}><div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'start' }}><div><div style={eyebrowStyle}>{eyebrow}</div><div style={{ marginTop: 5, fontFamily: serif, fontSize: 26, letterSpacing: '-0.04em' }}>{title}</div></div><span style={{ color: '#4b5545', fontFamily: serif, fontSize: 28 }}>{index}</span></div>{children}</div>
}

function Readout({ label, value, monoValue }: { label: string; value: string; monoValue?: boolean }) {
  return <div style={{ display: 'grid', gap: 5, padding: '13px 14px 13px 0', borderRight: `1px solid ${colors.line}` }}><div style={labelStyle}>{label}</div><strong style={{ fontFamily: monoValue ? mono : serif, fontSize: monoValue ? 14 : 21, fontWeight: 400 }}>{value}</strong></div>
}

function Direction({ label, value, outbound }: { label: string; value: string; outbound?: boolean }) {
  return <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: 10, background: colors.deep }}><span style={{ display: 'grid', width: 28, height: 28, placeItems: 'center', border: `1px solid ${outbound ? '#5c5473' : '#61734f'}`, color: outbound ? colors.violet : colors.lime, fontFamily: mono, fontSize: 9 }}>{label}</span><div><div style={labelStyle}>{outbound ? 'Outbound' : 'Inbound'}</div><strong style={{ fontSize: 13 }}>{value}</strong></div></div>
}

function LoadRow({ label, value, width }: { label: string; value: string; width: number }) {
  return <div style={{ display: 'grid', gridTemplateColumns: '28px 1fr 35px', alignItems: 'center', gap: 8 }}><span style={{ color: colors.muted, fontFamily: mono, fontSize: 10 }}>{label}</span><div style={{ height: 5, background: '#30352d' }}><div style={{ width: `${width}%`, height: '100%', background: colors.lime }} /></div><strong style={{ color: colors.ink, fontFamily: mono, fontSize: 10, textAlign: 'right' }}>{value}</strong></div>
}

function Compact({ label, value }: { label: string; value: string }) {
  return <div style={{ display: 'grid', gap: 5, padding: '10px 9px 10px 0' }}><span style={labelStyle}>{label}</span><strong style={{ fontSize: 12 }}>{value}</strong></div>
}

function Status({ label, color }: { label: string; color: string }) {
  return <span style={{ padding: '4px 6px', border: `1px solid ${color}`, color, fontFamily: mono, fontSize: 9, letterSpacing: '0.06em', textTransform: 'uppercase' }}>{label}</span>
}

function Revision({ label, value, detail, accent }: { label: string; value: string; detail: string; accent?: string }) {
  return <div style={{ display: 'grid', gap: 5, padding: '14px 15px', borderRight: `1px solid ${colors.line}` }}><span style={labelStyle}>{label}</span><code style={{ color: accent ?? colors.ink, fontSize: 11 }}>{value}</code><span style={{ color: colors.dim, fontSize: 10 }}>{detail}</span></div>
}

function Field({ label, value, compact }: { label: string; value: string; compact?: boolean }) {
  return <div style={{ display: 'grid', gap: 6 }}><span style={labelStyle}>{label}</span><div style={{ minHeight: compact ? 27 : 36, display: 'flex', alignItems: 'center', padding: compact ? '6px 8px' : '8px 10px', border: `1px solid ${colors.lineBright}`, color: colors.ink, background: colors.deep, fontFamily: mono, fontSize: compact ? 10 : 11 }}>{value}</div></div>
}

const eyebrowStyle: CSSProperties = { color: colors.muted, fontFamily: mono, fontSize: 9, fontWeight: 700, letterSpacing: '0.12em', textTransform: 'uppercase' }
const labelStyle: CSSProperties = { color: colors.muted, fontFamily: mono, fontSize: 9, letterSpacing: '0.08em', textTransform: 'uppercase' }
const captionStyle: CSSProperties = { marginTop: 5, color: colors.muted, fontSize: 10 }
