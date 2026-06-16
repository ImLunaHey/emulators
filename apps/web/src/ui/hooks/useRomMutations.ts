import { useMutation, useQueryClient } from '@tanstack/react-query';
import { addRom, deleteRom } from '../romStore';
import { md5Hex } from '../hasheous';

// useRomMutations — add / delete operations against the IndexedDB
// ROM store, invalidating the rom-list query after each so the
// library re-renders. addRom computes the MD5 up-front so the
// freshly-added entry already has a hash when it reaches the list.
export function useRomMutations() {
  const qc = useQueryClient();
  const add = useMutation({
    mutationFn: async ({ filename, bytes }: { filename: string; bytes: Uint8Array }) => {
      const md5 = await md5Hex(bytes);
      return addRom(filename, bytes, md5);
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ['rom-list'] }),
  });
  const remove = useMutation({
    mutationFn: (id: string) => deleteRom(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['rom-list'] }),
  });
  return { add, remove };
}
