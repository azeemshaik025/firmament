"use client";

import { useEffect, useRef } from "react";

type NodePoint = {
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
  phase: number;
};

export function SignalField() {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return undefined;
    }

    const context = canvas.getContext("2d");
    if (!context) {
      return undefined;
    }

    let width = 0;
    let height = 0;
    let nodes: NodePoint[] = [];
    let frame = 0;
    let animationFrame = 0;

    const buildNodes = () => {
      const count = width < 760 ? 24 : 44;
      nodes = Array.from({ length: count }, (_, index) => {
        const band = index % 4;
        return {
          x: Math.random() * width,
          y: Math.random() * height,
          vx: (Math.random() - 0.5) * (0.11 + band * 0.025),
          vy: (Math.random() - 0.5) * (0.11 + band * 0.025),
          r: 1.2 + Math.random() * 2.4,
          phase: Math.random() * Math.PI * 2,
        };
      });
    };

    const resize = () => {
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      width = window.innerWidth;
      height = Math.max(window.innerHeight, 720);
      canvas.width = Math.floor(width * ratio);
      canvas.height = Math.floor(height * ratio);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      buildNodes();
    };

    const draw = () => {
      frame += 0.01;
      context.clearRect(0, 0, width, height);

      const grid = context.createLinearGradient(0, 0, width, height);
      grid.addColorStop(0, "rgba(181, 255, 94, 0.08)");
      grid.addColorStop(0.48, "rgba(44, 208, 181, 0.035)");
      grid.addColorStop(1, "rgba(244, 188, 71, 0.045)");
      context.fillStyle = grid;
      context.fillRect(0, 0, width, height);

      nodes.forEach((node) => {
        node.x += node.vx;
        node.y += node.vy;
        node.phase += 0.016;

        if (node.x < -20) node.x = width + 20;
        if (node.x > width + 20) node.x = -20;
        if (node.y < -20) node.y = height + 20;
        if (node.y > height + 20) node.y = -20;
      });

      for (let left = 0; left < nodes.length; left += 1) {
        for (let right = left + 1; right < nodes.length; right += 1) {
          const a = nodes[left];
          const b = nodes[right];
          const dx = a.x - b.x;
          const dy = a.y - b.y;
          const distance = Math.sqrt(dx * dx + dy * dy);
          const maxDistance = width < 760 ? 128 : 184;

          if (distance < maxDistance) {
            const alpha = (1 - distance / maxDistance) * 0.16;
            context.strokeStyle = `rgba(181, 255, 94, ${alpha})`;
            context.lineWidth = 1;
            context.beginPath();
            context.moveTo(a.x, a.y);
            context.lineTo(b.x, b.y);
            context.stroke();
          }
        }
      }

      nodes.forEach((node, index) => {
        const pulse = 0.34 + Math.sin(node.phase + frame) * 0.14;
        context.fillStyle =
          index % 6 === 0
            ? `rgba(44, 208, 181, ${pulse})`
            : `rgba(181, 255, 94, ${pulse})`;
        context.beginPath();
        context.arc(node.x, node.y, node.r, 0, Math.PI * 2);
        context.fill();
      });

      animationFrame = window.requestAnimationFrame(draw);
    };

    resize();
    draw();
    window.addEventListener("resize", resize);

    return () => {
      window.cancelAnimationFrame(animationFrame);
      window.removeEventListener("resize", resize);
    };
  }, []);

  return <canvas ref={canvasRef} className="signal-field" aria-hidden="true" />;
}
