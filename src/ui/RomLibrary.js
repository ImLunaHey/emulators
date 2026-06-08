import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { useEffect, useState } from 'react';
import { addRom, deleteRom, listRoms } from './romStore';
export function RomLibrary({ open, currentId, onClose, onSelect, onAppend }) {
    const [roms, setRoms] = useState([]);
    const [busy, setBusy] = useState(false);
    const [dragging, setDragging] = useState(false);
    const refresh = async () => setRoms(await listRoms());
    useEffect(() => { if (open)
        refresh(); }, [open]);
    const handleFiles = async (files) => {
        setBusy(true);
        try {
            const arr = Array.from(files);
            for (const file of arr) {
                if (!file.name.toLowerCase().endsWith('.gba')) {
                    onAppend(`skipped ${file.name} (not a .gba file)`);
                    continue;
                }
                const bytes = new Uint8Array(await file.arrayBuffer());
                if (bytes.length < 0xC0) {
                    onAppend(`skipped ${file.name} (too small to be a GBA ROM)`);
                    continue;
                }
                const meta = await addRom(file.name, bytes);
                onAppend(`imported ${meta.title || meta.code} (${(bytes.length / (1024 * 1024)).toFixed(1)} MB)`);
            }
            await refresh();
        }
        catch (e) {
            onAppend(`ROM import failed: ${e.message}`);
        }
        finally {
            setBusy(false);
        }
    };
    const onDelete = async (id, title) => {
        if (!confirm(`Remove "${title}" from your library?`))
            return;
        await deleteRom(id);
        onAppend(`removed ${title}`);
        await refresh();
    };
    if (!open)
        return null;
    return (_jsx("div", { className: "fixed inset-0 bg-black/70 flex items-center justify-center z-[1000]", onClick: onClose, children: _jsxs("div", { className: "bg-[#14141a] border border-[#2a2a30] rounded-lg p-4 w-[640px] max-h-[80vh] overflow-y-auto shadow-2xl", onClick: (e) => e.stopPropagation(), children: [_jsxs("div", { className: "flex justify-between items-center mb-3 pb-2 border-b border-[#2a2a30]", children: [_jsx("div", { className: "text-sm font-bold tracking-wider", children: "ROM Library" }), _jsx("button", { onClick: onClose, className: "bg-transparent border-0 text-[#d8d8e0] text-lg cursor-pointer px-2 hover:text-white", children: "\u00D7" })] }), _jsxs("label", { className: `block border-2 border-dashed rounded-md p-6 text-center cursor-pointer mb-3 transition-colors ${dragging ? 'border-[#5060a0] bg-[#1c1c2a]' : 'border-[#2a2a30] hover:border-[#404050]'} ${busy ? 'opacity-50 pointer-events-none' : ''}`, onDragOver: (e) => { e.preventDefault(); setDragging(true); }, onDragLeave: () => setDragging(false), onDrop: (e) => { e.preventDefault(); setDragging(false); handleFiles(e.dataTransfer.files); }, children: [_jsx("input", { type: "file", accept: ".gba", multiple: true, className: "hidden", disabled: busy, onChange: (e) => { if (e.target.files) {
                                handleFiles(e.target.files);
                                e.target.value = '';
                            } } }), _jsx("div", { className: "text-xs opacity-80", children: busy ? 'Importing…' : 'Drop a .gba ROM here, or click to pick one' }), _jsx("div", { className: "text-[10px] opacity-50 mt-1", children: "Stored locally in your browser via IndexedDB \u2014 never uploaded anywhere" })] }), roms.length === 0 ? (_jsxs("div", { className: "py-8 text-center opacity-50 text-xs", children: ["No ROMs imported yet.", _jsx("br", {}), "Drop a .gba file above to get started."] })) : (_jsx("ul", { className: "space-y-1", children: roms.map((rom) => (_jsxs("li", { className: `flex items-center gap-3 p-2 rounded-md cursor-pointer transition-colors ${rom.id === currentId
                            ? 'bg-[#2a3a5a] border border-[#4060a0]'
                            : 'bg-[#1c1c22] border border-[#2a2a30] hover:bg-[#24242a]'}`, onClick: () => onSelect(rom), children: [_jsxs("div", { className: "flex-1 min-w-0", children: [_jsx("div", { className: "text-xs font-medium truncate", children: rom.title || rom.filename }), _jsxs("div", { className: "text-[10px] opacity-60 truncate", children: [rom.code, " \u00B7 ", (rom.size / (1024 * 1024)).toFixed(1), " MB \u00B7 ", rom.filename] })] }), rom.id === currentId && (_jsx("div", { className: "text-[10px] text-[#9be7ff] tracking-wider", children: "PLAYING" })), _jsx("button", { onClick: (e) => { e.stopPropagation(); onDelete(rom.id, rom.title || rom.filename); }, className: "bg-transparent border-0 text-[#9a9aa6] text-sm cursor-pointer px-2 hover:text-red-400", title: "Remove from library", children: "\uD83D\uDDD1" })] }, rom.id))) }))] }) }));
}
