interface SparkLineProps {
  data: number[];
  width?: number;
  height?: number;
  color?: string;
  className?: string;
}

export function SparkLine({
  data,
  width = 80,
  height = 16,
  color = "#4A7C59",
  className = "",
}: SparkLineProps) {
  if (data.length < 2) return null;

  const max = Math.max(...data, 1);
  const padding = 1;
  const effectiveWidth = width - padding * 2;
  const effectiveHeight = height - padding * 2;

  const points = data.map((value, i) => {
    const x = padding + (i / (data.length - 1)) * effectiveWidth;
    const y = padding + effectiveHeight - (value / max) * effectiveHeight;
    return `${x},${y}`;
  });

  const areaPoints = [
    `${padding},${height - padding}`,
    ...points,
    `${width - padding},${height - padding}`,
  ];

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className={className}
      aria-label={`Sparkline: ${data.join(", ")}`}
    >
      <polygon
        points={areaPoints.join(" ")}
        fill={color}
        fillOpacity={0.15}
      />
      <polyline
        points={points.join(" ")}
        fill="none"
        stroke={color}
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
