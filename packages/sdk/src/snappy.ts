const BLOCK_SIZE = 65536;
const HASH_BITS = 14;
const HASH_SIZE = 1 << HASH_BITS;
const MAX_LENGTH = 0xffff_ffff;
const MATCH_TABLE = new Uint32Array(HASH_SIZE);

function corrupt(message: string): Error {
    const error = new Error(`invalid Snappy block: ${message}`);
    error.name = 'CorruptionError';
    return error;
}

function word(bytes: Uint8Array, offset: number): number {
    return (bytes[offset] | (bytes[offset + 1] << 8) | (bytes[offset + 2] << 16) | (bytes[offset + 3] << 24)) >>> 0;
}

function hash(value: number, bits: number): number {
    return Math.imul(value, 0x9e3779b1) >>> (32 - bits);
}

export function encodeSnappy(input: Uint8Array): Uint8Array {
    if (input.byteLength > MAX_LENGTH) {
        throw new RangeError('Snappy input exceeds the 32-bit length limit');
    }
    const output = new Uint8Array(input.byteLength + Math.ceil(input.byteLength / 6) + 32);
    let written = 0;
    let remainingLength = input.byteLength;
    while (remainingLength >= 128) {
        output[written++] = (remainingLength & 0x7f) | 0x80;
        remainingLength = Math.floor(remainingLength / 128);
    }
    output[written++] = remainingLength;

    const literal = (start: number, end: number): void => {
        if (start === end) {
            return;
        }
        let length = end - start - 1;
        if (length < 60) {
            output[written++] = length << 2;
        } else {
            const lengthBytes = length <= 0xff ? 1 : length <= 0xffff ? 2 : length <= 0xffffff ? 3 : 4;
            output[written++] = (59 + lengthBytes) << 2;
            for (let index = 0; index < lengthBytes; index += 1) {
                output[written++] = length & 0xff;
                length >>>= 8;
            }
        }
        output.set(input.subarray(start, end), written);
        written += end - start;
    };
    const copy = (offset: number, length: number): void => {
        while (length > 0) {
            const count = Math.min(length, 64);
            if (count >= 4 && count <= 11 && offset < 2048) {
                output[written++] = 1 | ((count - 4) << 2) | ((offset >>> 8) << 5);
                output[written++] = offset & 0xff;
            } else {
                output[written++] = 2 | ((count - 1) << 2);
                output[written++] = offset & 0xff;
                output[written++] = offset >>> 8;
            }
            length -= count;
        }
    };

    let hashBits = 8;
    while (1 << hashBits < Math.min(input.byteLength, HASH_SIZE)) {
        hashBits += 1;
    }
    const tableSize = 1 << hashBits;
    // Encoding is synchronous. Calls can reuse this bounded scratch table.
    const table = MATCH_TABLE;
    for (let blockStart = 0; blockStart < input.byteLength; blockStart += BLOCK_SIZE) {
        const blockEnd = Math.min(blockStart + BLOCK_SIZE, input.byteLength);
        table.fill(0, 0, tableSize);
        let position = blockStart;
        let literalStart = blockStart;
        let misses = 0;
        while (position + 4 <= blockEnd) {
            const current = word(input, position);
            const slot = hash(current, hashBits);
            const previous = table[slot];
            table[slot] = position - blockStart + 1;
            const candidate = blockStart + previous - 1;
            if (previous === 0 || current !== word(input, candidate)) {
                misses += 1;
                position += 1 + Math.min(misses >>> 5, 64);
                continue;
            }
            literal(literalStart, position);
            const matchStart = position;
            position += 4;
            while (position < blockEnd && input[position] === input[candidate + position - matchStart]) {
                position += 1;
            }
            copy(matchStart - candidate, position - matchStart);
            literalStart = position;
            misses = 0;
            if (position + 4 <= blockEnd) {
                table[hash(word(input, position - 1), hashBits)] = position - blockStart;
            }
        }
        literal(literalStart, blockEnd);
    }
    return output.subarray(0, written);
}

export function decodeSnappy(input: Uint8Array, maxOutputBytes: number, expectedOutputBytes?: number): Uint8Array {
    if (!Number.isSafeInteger(maxOutputBytes) || maxOutputBytes < 0 || maxOutputBytes > MAX_LENGTH) {
        throw new RangeError('invalid Snappy output limit');
    }
    let read = 0;
    let length = 0;
    for (let index = 0; ; index += 1) {
        if (read >= input.byteLength || index === 5) {
            throw corrupt('truncated or invalid length');
        }
        const byte = input[read++];
        if (index === 4 && byte > 0x0f) {
            throw corrupt('length exceeds 32 bits');
        }
        length += (byte & 0x7f) * 2 ** (index * 7);
        if (byte < 128) {
            break;
        }
    }
    if (length > maxOutputBytes) {
        throw corrupt(`output exceeds ${maxOutputBytes} byte limit`);
    }
    if (expectedOutputBytes !== undefined && length !== expectedOutputBytes) {
        throw corrupt(`length mismatch: expected ${expectedOutputBytes} bytes, got ${length}`);
    }
    const output = new Uint8Array(length);
    let written = 0;
    while (read < input.byteLength) {
        const tag = input[read++];
        const kind = tag & 3;
        if (kind === 0) {
            let count = tag >>> 2;
            if (count < 60) {
                count += 1;
            } else {
                const lengthBytes = count - 59;
                if (lengthBytes > input.byteLength - read) {
                    throw corrupt('truncated literal length');
                }
                count = 1;
                for (let index = 0; index < lengthBytes; index += 1) {
                    count += input[read++] * 2 ** (index * 8);
                }
            }
            if (count > input.byteLength - read || count > length - written) {
                throw corrupt('literal exceeds input or output');
            }
            output.set(input.subarray(read, read + count), written);
            read += count;
            written += count;
            continue;
        }
        const offsetBytes = kind === 1 ? 1 : kind === 2 ? 2 : 4;
        if (offsetBytes > input.byteLength - read) {
            throw corrupt('truncated copy offset');
        }
        const count = kind === 1 ? 4 + ((tag >>> 2) & 7) : 1 + (tag >>> 2);
        let offset = kind === 1 ? (tag & 0xe0) << 3 : 0;
        for (let index = 0; index < offsetBytes; index += 1) {
            offset += input[read++] * 2 ** (index * 8);
        }
        if (offset === 0 || offset > written || count > length - written) {
            throw corrupt('copy exceeds output or has an invalid offset');
        }
        let copied = 0;
        // Overlapping copies repeat the decoded prefix. Each step doubles it.
        while (copied < count) {
            const chunk = Math.min(offset + copied, count - copied);
            output.copyWithin(written + copied, written - offset, written - offset + chunk);
            copied += chunk;
        }
        written += count;
    }
    if (written !== length) {
        throw corrupt(`length mismatch: expected ${length} bytes, got ${written}`);
    }
    return output;
}
