/** Valores que podem ir como parâmetro `?` ou voltar numa linha. */
export type MateValue = number | bigint | string | boolean | null;

/** Uma linha como objeto `{ coluna: valor }`. */
export type Row = Record<string, MateValue>;

export type MateErrorCode =
  | "IO"
  | "CORRUPT"
  | "PARSE"
  | "PLAN"
  | "EXECUTION"
  | "CONSTRAINT"
  | "TRANSACTION"
  | "BUSY"
  | "CLOSED";

export class MateError extends Error {
  readonly code: MateErrorCode;
}

export class Database {
  /** Abre (ou cria) o banco no arquivo indicado. */
  constructor(filename: string);
  readonly filename: string;
  /** Há uma transação aberta? */
  readonly inTransaction: boolean;

  /** Executa um ou mais comandos separados por `;`. Sem parâmetros. */
  exec(sql: string): this;
  /** INSERT/UPDATE/DELETE: devolve quantas linhas foram afetadas. */
  run(sql: string, params?: MateValue[]): { changes: number };
  /** Todas as linhas, como objetos. */
  all<T = Row>(sql: string, params?: MateValue[]): T[];
  /** A primeira linha, ou `undefined`. */
  get<T = Row>(sql: string, params?: MateValue[]): T | undefined;
  /** As linhas como arrays, com os nomes das colunas. */
  values(sql: string, params?: MateValue[]): { columns: string[]; rows: MateValue[][] };
  /** Embrulha `fn` numa transação: COMMIT se terminar, ROLLBACK se lançar erro. */
  transaction<A extends unknown[], R>(fn: (...args: A) => R): (...args: A) => R;
  /** Fecha o banco e libera o arquivo. */
  close(): void;
}
