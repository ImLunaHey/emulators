import { jsx as _jsx } from "react/jsx-runtime";
import { useEffect, useRef } from 'react';
export function LogPane({ lines }) {
    const ref = useRef(null);
    useEffect(() => {
        if (ref.current)
            ref.current.scrollTop = ref.current.scrollHeight;
    }, [lines]);
    return (_jsx("div", { ref: ref, className: "w-[720px] h-[120px] overflow-auto bg-[#0e0e12] border border-[#1c1c20] p-2 text-[11px] text-[var(--color-muted)] whitespace-pre-wrap", children: lines.map((l, i) => _jsx("div", { children: l }, i)) }));
}
