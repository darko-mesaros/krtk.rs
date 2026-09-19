module.exports = {
  testEnvironment: 'node',
  roots: ['<rootDir>/test'],
  // lib/*.js is ignored build output. Resolve .ts first so a test validates the same
  // CDK source that `ts-node --prefer-ts-exts` deploys, not a stale sidecar.
  moduleFileExtensions: ['ts', 'tsx', 'js', 'jsx', 'json', 'node'],
  testMatch: ['**/*.test.ts'],
  transform: {
    '^.+\\.tsx?$': 'ts-jest'
  }
};
