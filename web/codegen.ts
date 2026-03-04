import type { CodegenConfig } from '@graphql-codegen/cli'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))

const config: CodegenConfig = {
  schema: resolve(__dirname, '../schema.graphql'),
  generates: {
    [resolve(__dirname, 'src/lib/generated/schema.ts')]: {
      plugins: ['typescript'],
      config: {
        scalars: {
          DateTime: 'string',
          JSON: 'unknown',
        },
        defaultScalarType: 'unknown',
        strictScalars: true,
        useTypeImports: true,
        enumsAsTypes: true,
        avoidOptionals: {
          field: true,
        },
      },
    },
  },
}

export default config
