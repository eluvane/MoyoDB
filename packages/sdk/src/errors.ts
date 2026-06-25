export class MoyoDbError extends Error {
    constructor(name: string, message: string) {
        super(message);
        this.name = name;
        Object.setPrototypeOf(this, new.target.prototype);
    }
}
export class UnsupportedPlatformError extends MoyoDbError {
    constructor(message: string) {
        super('UnsupportedPlatformError', message);
    }
}
export class DatabaseBusyError extends MoyoDbError {
    constructor(message: string) {
        super('DatabaseBusyError', message);
    }
}
export class CorruptionError extends MoyoDbError {
    constructor(message: string) {
        super('CorruptionError', message);
    }
}
export class StoreExistsError extends MoyoDbError {
    constructor(message: string) {
        super('StoreExistsError', message);
    }
}
export class StoreNotFoundError extends MoyoDbError {
    constructor(message: string) {
        super('StoreNotFoundError', message);
    }
}
export class IndexNotFoundError extends MoyoDbError {
    constructor(message: string) {
        super('IndexNotFoundError', message);
    }
}
export class ConstraintError extends MoyoDbError {
    constructor(message: string) {
        super('ConstraintError', message);
    }
}
export class UniqueIndexConstraintError extends ConstraintError {
    constructor(message: string) {
        super(message);
        this.name = 'UniqueIndexConstraintError';
    }
}
export class WriteTransactionAlreadyOpenError extends MoyoDbError {
    constructor(message = 'write transaction already open') {
        super('WriteTransactionAlreadyOpenError', message);
    }
}
export class ReadonlyTransactionError extends MoyoDbError {
    constructor(message = 'readonly transaction cannot perform this operation') {
        super('ReadonlyTransactionError', message);
    }
}
export class TransactionClosedError extends MoyoDbError {
    constructor(message = 'transaction is already closed') {
        super('TransactionClosedError', message);
    }
}
export class DatabaseClosedError extends MoyoDbError {
    constructor(message = 'database handle is closed') {
        super('DatabaseClosedError', message);
    }
}
export class ValueTooLargeError extends MoyoDbError {
    constructor(message: string) {
        super('ValueTooLargeError', message);
    }
}
export class KeyTooLargeError extends MoyoDbError {
    constructor(message: string) {
        super('KeyTooLargeError', message);
    }
}
export class StoreNameTooLongError extends MoyoDbError {
    constructor(message: string) {
        super('StoreNameTooLongError', message);
    }
}
export class ReservedStoreNameError extends MoyoDbError {
    constructor(message: string) {
        super('ReservedStoreNameError', message);
    }
}
export class InvalidRangeError extends MoyoDbError {
    constructor(message: string) {
        super('InvalidRangeError', message);
    }
}
export class StorageError extends MoyoDbError {
    constructor(message: string) {
        super('StorageError', message);
    }
}
export class SerializationError extends MoyoDbError {
    constructor(message: string) {
        super('SerializationError', message);
    }
}
export class InjectedFailureError extends MoyoDbError {
    constructor(message: string) {
        super('InjectedFailureError', message);
    }
}
export class ChangeFeedCompactedError extends MoyoDbError {
    constructor(message: string) {
        super('ChangeFeedCompactedError', message);
    }
}
export class InternalError extends MoyoDbError {
    constructor(message: string) {
        super('InternalError', message);
    }
}
export class InvalidOpenOptionsError extends MoyoDbError {
    constructor(message: string) {
        super('InvalidOpenOptionsError', message);
    }
}
export class VersionError extends MoyoDbError {
    constructor(message: string) {
        super('VersionError', message);
    }
}
type ErrorPayload = {
    code?: string;
    name?: string;
    message?: string;
};
type ErrorConstructor = new (message: string) => Error;
const ERROR_CONSTRUCTORS = {
    UnsupportedPlatformError,
    DatabaseBusyError,
    CorruptionError,
    StoreExistsError,
    StoreNotFoundError,
    IndexNotFoundError,
    ConstraintError,
    UniqueIndexConstraintError,
    WriteTransactionAlreadyOpenError,
    ReadonlyTransactionError,
    TransactionClosedError,
    DatabaseClosedError,
    ValueTooLargeError,
    KeyTooLargeError,
    StoreNameTooLongError,
    ReservedStoreNameError,
    InvalidRangeError,
    StorageError,
    SerializationError,
    InjectedFailureError,
    ChangeFeedCompactedError,
    InternalError,
    InvalidOpenOptionsError,
    VersionError
} satisfies Record<string, ErrorConstructor>;

function instantiateError(code: string, message: string): Error {
    const ErrorConstructor = ERROR_CONSTRUCTORS[code as keyof typeof ERROR_CONSTRUCTORS];
    if (ErrorConstructor) {
        return new ErrorConstructor(message);
    }
    const err = new Error(message);
    err.name = code;
    return err;
}

export function normalizeError(error: unknown): Error {
    if (error instanceof MoyoDbError) {
        return error;
    }
    if (error instanceof Error) {
        if (error.name === 'Error') {
            return error;
        }
        return instantiateError(error.name, error.message);
    }
    const payload = error as ErrorPayload | undefined;
    const code = payload?.code ?? payload?.name ?? 'Error';
    const message = payload?.message ?? String(error);
    return instantiateError(code, message);
}
