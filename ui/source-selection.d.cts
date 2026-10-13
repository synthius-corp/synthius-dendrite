export const DIRECTORY_INPUT_ATTRIBUTES: { readonly webkitdirectory: ''; readonly directory: ''; readonly mozdirectory: '' };
export function normalizeSelectedUploadFiles(files: File[]): File[];
export function collectDroppedUploadFiles(transfer: DataTransfer): Promise<File[]>;
export function wrapFileWithFlattenedName(file: File, pathLike?: string): File;
